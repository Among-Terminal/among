//! Declarative status content and the slots it is shown in.
//!
//! Plugins never construct interface objects. They return [`Fragment`] values —
//! plain data — and this module turns them into widgets. That keeps the plugin
//! boundary free of any ABI surface and means a plugin cannot break the layout
//! by producing something oversized or malformed.

use std::collections::BTreeMap;

use gtk::prelude::*;
use gtk::{gdk, glib};

/// Where a piece of content belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Header above the whole window.
    AppHeader,
    /// Footer below the whole window.
    AppFooter,
    /// Header of the active workspace's pane area.
    WorkspaceHeader,
    /// Header inside one pane, above its terminal.
    PaneHeader(crate::tree::PaneId),
    /// Footer inside one pane, below its terminal.
    PaneFooter(crate::tree::PaneId),
}

impl Scope {
    /// True when the scope refers to one specific pane.
    #[cfg(test)]
    pub fn is_pane(self) -> bool {
        matches!(self, Scope::PaneHeader(_) | Scope::PaneFooter(_))
    }

    /// The pane this scope belongs to, if any.
    pub fn pane(self) -> Option<crate::tree::PaneId> {
        match self {
            Scope::PaneHeader(id) | Scope::PaneFooter(id) => Some(id),
            _ => None,
        }
    }
}

/// A visual style for a fragment, mapped onto GTK's named styles.
///
/// A closed set on purpose: plugins pick a style, they do not choose colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Style {
    Default,
    Success,
    Warning,
    Error,
    Accent,
    Dim,
}

impl Style {
    /// The GTK CSS class this style renders with.
    pub fn css_class(self) -> &'static str {
        match self {
            Style::Default => "status",
            Style::Success => "status success",
            Style::Warning => "status warning",
            Style::Error => "status error",
            Style::Accent => "status accent",
            Style::Dim => "status dim",
        }
    }
}

/// How often a plugin's data changes.
///
/// Lets the host avoid re-asking for values that cannot have changed, which
/// matters because collecting them can be expensive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// Data changes on terminal events rather than on a timer.
    OnEvent,
    /// Data changes at most this often.
    IntervalMs(u64),
}

/// One piece of content. Pure data: no widget types appear here.
#[derive(Debug, Clone, PartialEq)]
pub struct Fragment {
    pub meter: Option<f64>,
    pub text: Option<String>,
    pub icon: Option<String>,
    pub style: Style,
    pub tooltip: Option<String>,
    /// Lower sorts first within a slot.
    pub priority: i32,
    /// Identity of the contributing plugin, used as a stable tie-break.
    pub source: String,
}

impl Fragment {
    #[cfg(test)]
    pub fn text(source: &str, text: impl Into<String>) -> Self {
        Self {
            meter: None,
            text: Some(text.into()),
            icon: None,
            style: Style::Default,
            tooltip: None,
            priority: 100,
            source: source.to_string(),
        }
    }

    #[cfg(test)]
    pub fn with_priority(mut self, priority: i32) -> Self {
        self.priority = priority;
        self
    }

    /// Whether this fragment would render as anything visible.
    fn is_renderable(&self) -> bool {
        self.text.as_deref().is_some_and(|t| !t.is_empty())
            || self.icon.as_deref().is_some_and(|i| !i.is_empty())
    }
}

/// Compact content shown next to a workspace's name in the sidebar.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub icon: Option<String>,
    pub text: Option<String>,
    pub tooltip: Option<String>,
    pub style: Style,
    pub source: String,
}

impl Summary {
    #[cfg(test)]
    pub fn text(source: &str, text: impl Into<String>) -> Self {
        Self {
            icon: None,
            text: Some(text.into()),
            tooltip: None,
            style: Style::Default,
            source: source.to_string(),
        }
    }
}

/// One plugin's contribution to one slot, with its refresh policy.
#[derive(Debug, Clone)]
pub struct Contribution {
    /// Which plugin this belongs to.
    ///
    /// Carried explicitly rather than inferred from the fragments, because a
    /// plugin that legitimately reports nothing has no fragment to read a name
    /// from, and would otherwise accumulate a duplicate entry per response.
    pub source: String,
    pub fragments: Vec<Fragment>,
    pub refresh: Refresh,
    /// Last time the host asked for this data, as a monotonic millisecond count.
    pub last_asked_ms: Option<u64>,
}

/// Everything plugins have contributed, keyed by scope.
///
/// A `BTreeMap` keyed by scope keeps rendering order deterministic, which the
/// ordering requirement depends on.
#[derive(Debug, Clone, Default)]
pub struct SlotStore {
    slots: BTreeMap<Scope, Vec<Contribution>>,
    summaries: BTreeMap<(usize, String), Summary>,
}

impl SlotStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace a plugin's contribution to a slot.
    ///
    /// Replacing wholesale rather than appending is what lets a plugin's
    /// content disappear when it stops reporting, instead of lingering stale.
    ///
    /// A slot accepts only so many sources: past the cap the lowest-priority
    /// source is dropped, because one plugin flooding a slot must not be able to
    /// crowd out everyone else's contribution entirely.
    pub fn set(&mut self, scope: Scope, source: &str, contribution: Contribution) {
        let entry = self.slots.entry(scope).or_default();
        match entry.iter_mut().find(|c| c.source == source) {
            Some(existing) => *existing = contribution,
            None => entry.push(contribution),
        }
        // Enforced after the insert, since the cap is about what the slot ends
        // up holding rather than what it held a moment ago.
        self.enforce_source_cap(scope);
    }

    /// Per-source fragment counts in a slot, for diagnostics.
    pub fn sources_in(&self, scope: Scope) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        for contribution in self.slots.get(&scope).into_iter().flatten() {
            if contribution.fragments.is_empty() {
                let present = out.iter().any(|(name, _)| *name == contribution.source);
                if !present {
                    out.push((contribution.source.clone(), 0));
                }
                continue;
            }
            for fragment in &contribution.fragments {
                match out.iter_mut().find(|(name, _)| *name == fragment.source) {
                    Some(entry) => entry.1 += 1,
                    None => out.push((fragment.source.clone(), 1)),
                }
            }
        }
        out.sort();
        out
    }

    /// Drop the lowest-priority source while a slot holds more than the cap.
    fn enforce_source_cap(&mut self, scope: Scope) {
        let cap = crate::plugin::protocol::MAX_SOURCES_PER_SLOT;
        let Some(contributions) = self.slots.get_mut(&scope) else {
            return;
        };
        while contributions.len() > cap {
            // Sorted strongest first so that popping the last entry removes the
            // weakest contributor — a plugin with low-priority content is the
            // one that yields its place, not the one that got there first.
            contributions.sort_by_key(|c| {
                std::cmp::Reverse(
                    c.fragments
                        .iter()
                        .map(|f| f.priority)
                        .min()
                        .unwrap_or(i32::MAX),
                )
            });
            contributions.pop();
        }
    }

    /// Remove a plugin's contribution from every slot and summary.
    pub fn remove_source(&mut self, source: &str) {
        for contributions in self.slots.values_mut() {
            contributions.retain(|c| c.source != source);
        }
        self.slots.retain(|_, v| !v.is_empty());
        // A sidebar summary from a plugin that stopped working is as stale as
        // its slot content, so it is withdrawn too.
        self.summaries.retain(|(_, owner), _| owner != source);
    }

    /// Drop all content belonging to panes that no longer exist.
    pub fn retain_panes(&mut self, live: &[crate::tree::PaneId]) {
        self.slots.retain(|scope, _| match scope.pane() {
            None => true,
            Some(id) => live.contains(&id),
        });
    }

    /// The fragments visible in a slot, in display order.
    ///
    /// Ordering is by priority then by source, so a plugin that re-sends the
    /// same data never shuffles the slot around.
    pub fn fragments(&self, scope: Scope) -> Vec<&Fragment> {
        let mut out: Vec<&Fragment> = self
            .slots
            .get(&scope)
            .into_iter()
            .flatten()
            .flat_map(|c| c.fragments.iter())
            .filter(|f| f.is_renderable())
            .collect();
        out.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.source.cmp(&b.source))
        });
        out
    }

    /// Whether a scope currently holds content from any plugin.
    pub fn is_empty(&self, scope: Scope) -> bool {
        self.fragments(scope).is_empty()
    }

    pub fn set_summary(&mut self, workspace: usize, summary: Summary) {
        let key = (workspace, summary.source.clone());
        self.summaries.insert(key, summary);
    }

    #[cfg(test)]
    pub fn remove_summary(&mut self, workspace: usize, source: &str) {
        self.summaries
            .retain(|(index, owner), _| !(*index == workspace && owner == source));
    }

    /// Summaries for one workspace, ordered by source for a stable sidebar.
    pub fn summaries(&self, workspace: usize) -> Vec<&Summary> {
        let mut out: Vec<&Summary> = self
            .summaries
            .iter()
            .filter(|((index, _), _)| *index == workspace)
            .map(|(_, summary)| summary)
            .collect();
        out.sort_by(|a, b| a.source.cmp(&b.source));
        out
    }

    /// Drop summaries belonging to workspaces that no longer exist.
    pub fn retain_workspaces(&mut self, live_count: usize) {
        self.summaries.retain(|(index, _), _| *index < live_count);
    }

    /// Whether a slot needs collecting from plugins.
    ///
    /// Distinct from [`Self::needs_refresh`]: a slot nobody has contributed to
    /// yet still has to be asked about, otherwise the first request would never
    /// be sent and the slot would stay empty forever.
    pub fn needs_collection(&self, scope: Scope, now_ms: u64) -> bool {
        match self.slots.get(&scope) {
            None => true,
            Some(contributions) if contributions.is_empty() => true,
            Some(_) => self.needs_refresh(scope, now_ms),
        }
    }

    /// Whether a slot needs refreshing from its contributors.
    pub fn needs_refresh(&self, scope: Scope, now_ms: u64) -> bool {
        self.slots
            .get(&scope)
            .into_iter()
            .flatten()
            .any(|contribution| match contribution.refresh {
                Refresh::OnEvent => contribution.last_asked_ms.is_none(),
                Refresh::IntervalMs(interval) => match contribution.last_asked_ms {
                    None => true,
                    Some(last) => now_ms.saturating_sub(last) >= interval,
                },
            })
    }

    /// Mark a slot as freshly collected so the interval restarts.
    pub fn mark_collected(&mut self, scope: Scope, now_ms: u64) {
        if let Some(contributions) = self.slots.get_mut(&scope) {
            for contribution in contributions.iter_mut() {
                contribution.last_asked_ms = Some(now_ms);
            }
        }
    }
}

/// Truncate text so a fragment cannot take over a whole slot.
pub fn bound_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{keep}…")
}

/// Build a widget showing one fragment.
///
/// Returns `None` for content that cannot be rendered at all, rather than an
/// empty widget that would still take up layout space.
pub fn fragment_widget(fragment: &Fragment, max_chars: usize) -> Option<gtk::Widget> {
    let box_ = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    box_.add_css_class("status-fragment");

    let text = fragment.text.as_deref().map(|t| bound_text(t, max_chars));
    let icon = fragment.icon.as_deref().filter(|i| !i.is_empty());

    match (icon, text.as_deref()) {
        (None, None) => return None,
        (Some(name), None) => {
            let image = gtk::Image::from_icon_name(name);
            image.set_pixel_size(16);
            box_.append(&image);
        }
        (None, Some(label)) => {
            let lbl = gtk::Label::new(Some(label));
            lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            box_.append(&lbl);
        }
        (Some(name), Some(label)) => {
            let image = gtk::Image::from_icon_name(name);
            image.set_pixel_size(16);
            box_.append(&image);
            let lbl = gtk::Label::new(Some(label));
            lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            box_.append(&lbl);
        }
    }

    if let Some(tooltip) = &fragment.tooltip {
        box_.set_tooltip_text(Some(tooltip));
    }
    if let Some(fraction) = fragment.meter {
        let meter = gtk::ProgressBar::new();
        meter.add_css_class("status-meter");
        meter.set_fraction(fraction);
        meter.set_width_request(50);
        meter.set_valign(gtk::Align::Center);
        meter.set_focusable(false);
        let (red, green, blue) = meter_colour(fraction);
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&format!(
            "trough {{ background-image: none; background-color: rgba({red}, {green}, {blue}, 0.25); }} progress {{ background-image: none; background-color: rgb({red}, {green}, {blue}); }}"
        ));
        // A per-widget provider avoids registering a new display-wide sheet on
        // every status tick. GTK's internal progress node owns the fill paint.
        // GTK 4.12 has no replacement for scoped dynamic CSS; a display-wide
        // provider here would accumulate sheets or recolour unrelated meters.
        #[allow(deprecated)]
        fn apply_meter_colour(widget: &gtk::Widget, provider: &gtk::CssProvider) {
            widget
                .style_context()
                .add_provider(provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
            let mut child = widget.first_child();
            while let Some(node) = child {
                apply_meter_colour(&node, provider);
                child = node.next_sibling();
            }
        }
        apply_meter_colour(meter.upcast_ref(), &provider);
        meter.update_property(&[gtk::accessible::Property::Label(
            fragment.text.as_deref().unwrap_or("Utilization"),
        )]);
        box_.append(&meter);
    }
    for class in fragment.style.css_class().split_whitespace() {
        box_.add_css_class(class);
    }
    Some(box_.upcast())
}

/// Interpolate green → yellow → red without quantized threshold jumps.
fn meter_colour(fraction: f64) -> (u8, u8, u8) {
    let fraction = fraction.clamp(0.0, 1.0);
    let (start, end, weight) = if fraction <= 0.5 {
        ([46.0, 194.0, 126.0], [246.0, 211.0, 45.0], fraction * 2.0)
    } else {
        (
            [246.0, 211.0, 45.0],
            [224.0, 27.0, 36.0],
            (fraction - 0.5) * 2.0,
        )
    };
    let channels: [u8; 3] = std::array::from_fn(|index| {
        (start[index] + (end[index] - start[index]) * weight).round() as u8
    });
    (channels[0], channels[1], channels[2])
}

/// Whether an icon name resolves in the current theme.
///
/// Missing icons are dropped rather than rendered as a broken-image placeholder.
pub fn icon_exists(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let Some(display) = gdk::Display::default() else {
        return false;
    };
    gtk::IconTheme::for_display(&display).has_icon(name)
}

/// Render a slot's current content into a container, replacing what was there.
///
/// Rebuilt wholesale on each update: fragment counts are small, and rebuilding
/// avoids stale widgets from a plugin that stopped reporting.
pub fn render_slot(store: &SlotStore, scope: Scope, container: &gtk::Box, max_chars: usize) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    if scope == Scope::AppFooter && !store.is_empty(scope) {
        // Alignment belongs to the application's slot, not to plugin data.
        // Keep a full-width footer while its content sits at the right edge.
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        container.append(&spacer);
    }
    for fragment in store.fragments(scope) {
        if let Some(widget) = fragment_widget(fragment, max_chars) {
            container.append(&widget);
        }
    }
    container.set_visible(!store.is_empty(scope));
}

/// A monotonic millisecond counter for refresh scheduling.
///
/// `glib::get_monotonic_time` is used because wall-clock changes would make
/// refresh intervals behave unpredictably.
pub fn now_ms() -> u64 {
    (glib::monotonic_time() / 1_000).max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::PaneId;

    fn contribution(source: &str, fragments: Vec<Fragment>) -> Contribution {
        Contribution {
            source: source.to_string(),
            fragments,
            refresh: Refresh::IntervalMs(1000),
            last_asked_ms: None,
        }
    }

    #[test]
    fn scope_reports_its_pane() {
        assert_eq!(Scope::PaneHeader(PaneId(1)).pane(), Some(PaneId(1)));
        assert_eq!(Scope::PaneFooter(PaneId(1)).pane(), Some(PaneId(1)));
        assert_eq!(Scope::AppHeader.pane(), None);
        assert!(Scope::PaneHeader(PaneId(1)).is_pane());
        assert!(!Scope::WorkspaceHeader.is_pane());
    }

    #[test]
    fn a_new_store_is_empty() {
        let store = SlotStore::new();
        assert!(store.is_empty(Scope::AppHeader));
        assert!(store.fragments(Scope::AppHeader).is_empty());
    }

    #[test]
    fn fragments_land_in_the_requested_scope_only() {
        let mut store = SlotStore::new();
        store.set(
            Scope::PaneHeader(PaneId(1)),
            "git",
            contribution("git", vec![Fragment::text("git", "main")]),
        );
        assert_eq!(store.fragments(Scope::PaneHeader(PaneId(1))).len(), 1);
        assert!(store.is_empty(Scope::PaneHeader(PaneId(2))));
        assert!(store.is_empty(Scope::WorkspaceHeader));
        assert!(store.is_empty(Scope::AppHeader));
    }

    #[test]
    fn two_sources_coexist_in_one_slot() {
        let mut store = SlotStore::new();
        let scope = Scope::WorkspaceHeader;
        store.set(
            scope,
            "git",
            contribution("git", vec![Fragment::text("git", "main")]),
        );
        store.set(
            scope,
            "battery",
            contribution("battery", vec![Fragment::text("battery", "80%")]),
        );
        assert_eq!(store.fragments(scope).len(), 2);
    }

    #[test]
    fn one_plugin_does_not_truncate_another() {
        let mut store = SlotStore::new();
        let scope = Scope::WorkspaceHeader;
        let many: Vec<Fragment> = (0..50)
            .map(|i| Fragment::text("noisy", format!("{i}")))
            .collect();
        store.set(scope, "noisy", contribution("noisy", many));
        store.set(
            scope,
            "git",
            contribution("git", vec![Fragment::text("git", "main")]),
        );
        assert_eq!(store.fragments(scope).len(), 51);
    }

    #[test]
    fn higher_priority_sorts_first() {
        let mut store = SlotStore::new();
        let scope = Scope::AppFooter;
        store.set(
            scope,
            "late",
            contribution(
                "late",
                vec![Fragment::text("late", "late").with_priority(500)],
            ),
        );
        store.set(
            scope,
            "early",
            contribution(
                "early",
                vec![Fragment::text("early", "early").with_priority(1)],
            ),
        );
        let texts: Vec<&str> = store
            .fragments(scope)
            .iter()
            .map(|f| f.text.as_deref().unwrap())
            .collect();
        assert_eq!(texts, vec!["early", "late"]);
    }

    #[test]
    fn repeated_updates_preserve_order() {
        let mut store = SlotStore::new();
        let scope = Scope::AppFooter;
        store.set(
            scope,
            "a",
            contribution("a", vec![Fragment::text("a", "A").with_priority(10)]),
        );
        store.set(
            scope,
            "b",
            contribution("b", vec![Fragment::text("b", "B").with_priority(10)]),
        );
        let first: Vec<String> = store
            .fragments(scope)
            .iter()
            .map(|f| f.text.clone().unwrap())
            .collect();

        // An unrelated update elsewhere must not disturb this slot.
        store.set(
            Scope::AppHeader,
            "c",
            contribution("c", vec![Fragment::text("c", "C")]),
        );
        let second: Vec<String> = store
            .fragments(scope)
            .iter()
            .map(|f| f.text.clone().unwrap())
            .collect();
        assert_eq!(first, second);
    }

    #[test]
    fn resending_a_source_replaces_rather_than_appends() {
        let mut store = SlotStore::new();
        let scope = Scope::AppFooter;
        store.set(
            scope,
            "git",
            contribution("git", vec![Fragment::text("git", "main")]),
        );
        store.set(
            scope,
            "git",
            contribution("git", vec![Fragment::text("git", "dirty")]),
        );
        let texts: Vec<&str> = store
            .fragments(scope)
            .iter()
            .map(|f| f.text.as_deref().unwrap())
            .collect();
        assert_eq!(texts, vec!["dirty"]);
    }

    #[test]
    fn removing_a_source_withdraws_its_content_everywhere() {
        let mut store = SlotStore::new();
        store.set(
            Scope::AppHeader,
            "git",
            contribution("git", vec![Fragment::text("git", "x")]),
        );
        store.set(
            Scope::PaneFooter(PaneId(1)),
            "git",
            contribution("git", vec![Fragment::text("git", "y")]),
        );
        store.remove_source("git");
        assert!(store.is_empty(Scope::AppHeader));
        assert!(store.is_empty(Scope::PaneFooter(PaneId(1))));
    }

    #[test]
    fn content_for_a_dead_pane_is_dropped() {
        let mut store = SlotStore::new();
        store.set(
            Scope::PaneHeader(PaneId(1)),
            "git",
            contribution("git", vec![Fragment::text("git", "a")]),
        );
        store.set(
            Scope::PaneHeader(PaneId(2)),
            "git",
            contribution("git", vec![Fragment::text("git", "b")]),
        );
        store.retain_panes(&[PaneId(1)]);
        assert_eq!(store.fragments(Scope::PaneHeader(PaneId(1))).len(), 1);
        assert!(store.is_empty(Scope::PaneHeader(PaneId(2))));
    }

    #[test]
    fn summaries_accumulate_per_workspace() {
        let mut store = SlotStore::new();
        store.set_summary(0, Summary::text("terminals", "4"));
        store.set_summary(0, Summary::text("notifications", "2"));
        assert_eq!(store.summaries(0).len(), 2);
        assert!(store.summaries(1).is_empty());
    }

    #[test]
    fn removing_a_source_withdraws_its_summaries_too() {
        let mut store = SlotStore::new();
        store.set_summary(0, Summary::text("boom", "1"));
        store.set_summary(0, Summary::text("demo", "2"));
        store.remove_source("boom");
        let left = store.summaries(0);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].source, "demo");
    }

    #[test]
    fn removing_a_summary_leaves_the_others_selectable() {
        let mut store = SlotStore::new();
        store.set_summary(0, Summary::text("terminals", "4"));
        store.set_summary(0, Summary::text("notifications", "2"));
        store.remove_summary(0, "notifications");
        let left = store.summaries(0);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].source, "terminals");
    }

    #[test]
    fn summaries_for_a_closed_workspace_are_dropped() {
        let mut store = SlotStore::new();
        store.set_summary(0, Summary::text("terminals", "4"));
        store.set_summary(1, Summary::text("terminals", "9"));
        store.retain_workspaces(1);
        assert_eq!(store.summaries(0).len(), 1);
        assert!(store.summaries(1).is_empty());
    }

    #[test]
    fn a_plugin_reporting_nothing_still_occupies_one_entry() {
        let mut store = SlotStore::new();
        let scope = Scope::AppHeader;
        // A plugin with no cwd legitimately answers with no fragments; repeated
        // answers must replace its entry rather than accumulate duplicates.
        for _ in 0..10 {
            store.set(
                scope,
                "git",
                Contribution {
                    source: "git".into(),
                    fragments: Vec::new(),
                    refresh: Refresh::OnEvent,
                    last_asked_ms: None,
                },
            );
        }
        assert_eq!(store.sources_in(scope), vec![("git".to_string(), 0)]);
    }

    #[test]
    fn a_repeated_empty_answer_does_not_displace_another_plugin() {
        let mut store = SlotStore::new();
        let scope = Scope::AppHeader;
        store.set(
            scope,
            "demo",
            Contribution {
                source: "demo".into(),
                fragments: vec![Fragment::text("demo", "among")],
                refresh: Refresh::IntervalMs(2000),
                last_asked_ms: None,
            },
        );
        for _ in 0..5 {
            store.set(
                scope,
                "git",
                Contribution {
                    source: "git".into(),
                    fragments: Vec::new(),
                    refresh: Refresh::OnEvent,
                    last_asked_ms: None,
                },
            );
        }
        let visible = store.fragments(scope);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].text.as_deref(), Some("among"));
    }

    #[test]
    fn a_slot_refuses_more_sources_than_the_cap() {
        let mut store = SlotStore::new();
        let scope = Scope::AppFooter;
        // The last plugin has the weakest priority, so it is the one dropped.
        for index in 0..=crate::plugin::protocol::MAX_SOURCES_PER_SLOT {
            let fragment = Fragment::text(&format!("p{index:02}"), "x").with_priority(index as i32);
            store.set(
                scope,
                &format!("p{index:02}"),
                Contribution {
                    source: format!("p{index:02}"),
                    fragments: vec![fragment],
                    refresh: Refresh::OnEvent,
                    last_asked_ms: None,
                },
            );
        }
        let kept = store.fragments(scope);
        assert!(kept.len() <= crate::plugin::protocol::MAX_SOURCES_PER_SLOT);
        // The weakest contributor lost its slot; stronger ones survived.
        assert!(!kept.iter().any(|f| f.priority == 0));
        assert!(kept.iter().any(|f| f.priority == 32));
    }

    #[test]
    fn a_slot_with_no_fragments_reports_needs_refresh() {
        let store = SlotStore::new();
        assert!(!store.needs_refresh(Scope::AppHeader, 0));
    }

    #[test]
    fn an_unanswered_slot_still_needs_collecting() {
        let store = SlotStore::new();
        assert!(!store.needs_refresh(Scope::AppHeader, 0));
        // Nothing has contributed yet, so the host must still ask.
        assert!(store.needs_collection(Scope::AppHeader, 0));
    }

    #[test]
    fn a_contributed_slot_is_not_recollected_immediately() {
        let mut store = SlotStore::new();
        let scope = Scope::AppHeader;
        store.set(
            scope,
            "p",
            contribution("p", vec![Fragment::text("p", "x")]),
        );
        store.mark_collected(scope, 1000);
        assert!(!store.needs_collection(scope, 1500));
        assert!(store.needs_collection(scope, 3000));
    }

    #[test]
    fn interval_refresh_is_not_requested_before_the_interval_elapses() {
        let mut store = SlotStore::new();
        let scope = Scope::AppFooter;
        store.set(
            scope,
            "clock",
            contribution("clock", vec![Fragment::text("clock", "now")]),
        );
        assert!(store.needs_refresh(scope, 0));
        store.mark_collected(scope, 1000);
        assert!(!store.needs_refresh(scope, 1500));
        assert!(store.needs_refresh(scope, 2000));
    }

    #[test]
    fn event_driven_data_is_requested_when_it_has_never_been_collected() {
        let mut store = SlotStore::new();
        let scope = Scope::WorkspaceHeader;
        store.set(
            scope,
            "git",
            Contribution {
                source: "git".into(),
                fragments: vec![Fragment::text("git", "main")],
                refresh: Refresh::OnEvent,
                last_asked_ms: None,
            },
        );
        assert!(store.needs_refresh(scope, 0));
        store.mark_collected(scope, 1000);
        assert!(!store.needs_refresh(scope, 1001));
    }

    #[test]
    fn empty_and_unrenderable_fragments_are_filtered_out() {
        let mut store = SlotStore::new();
        let scope = Scope::AppHeader;
        store.set(
            scope,
            "x",
            contribution(
                "x",
                vec![Fragment::text("x", ""), Fragment::text("y", "ok")],
            ),
        );
        assert_eq!(store.fragments(scope).len(), 1);
    }

    #[test]
    fn overlong_text_is_bounded() {
        assert_eq!(bound_text("short", 10), "short");
        let bounded = bound_text("0123456789abcdef", 10);
        assert_eq!(bounded.chars().count(), 10);
        assert!(bounded.ends_with('…'));
    }

    #[test]
    fn bounding_counts_characters_not_bytes() {
        // A multi-byte string must not be cut mid-character. Terminal output is
        // rarely pure ASCII, so this is the common case rather than an edge one.
        let bounded = bound_text("café-naïve-ü", 6);
        assert_eq!(bounded.chars().count(), 6);
        assert!(
            !bounded.contains('\u{FFFD}'),
            "a partial character was emitted"
        );
    }
}
