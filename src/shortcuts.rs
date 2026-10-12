//! The keyboard action registry.
//!
//! One table maps actions to key sequences. Centralising it is what makes the
//! "inspect and rebind" requirement possible without touching call sites, and
//! it keeps the notion of "an action" in exactly one place.
//!
//! The default bindings were chosen to avoid anything GNOME reserves for its
//! own window and workspace management: `Ctrl+Alt+Arrow`, `Ctrl+Alt+Tab`,
//! `Alt+Tab` and every `Super` sequence are left alone.

use std::collections::BTreeMap;

/// An action the application can perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    /// Move focus to the neighbouring pane in a direction.
    FocusDirection(Direction),
    /// Activate the workspace at a position, 0-based.
    ActivateWorkspace(usize),
    /// Create a workspace.
    NewWorkspace,
    /// Split the focused pane.
    SplitPane,
    /// Move the divider bounding the focused pane one step in a direction.
    ResizeDivider(Direction),
    /// Close the focused pane.
    ClosePane,
    /// Open the preferences window.
    OpenPreferences,
    /// Toggle the window between fullscreen and its previous size.
    ToggleFullscreen,
    /// Expand the focused pane to fill the workspace, or restore the layout.
    ToggleZoom,
    /// Cycle to the next or previous workspace.
    CycleWorkspace(Cycle),
}

/// Direction of focus movement, matching [`crate::tree::Direction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

impl From<Direction> for crate::tree::Direction {
    fn from(direction: Direction) -> Self {
        match direction {
            Direction::Up => crate::tree::Direction::Up,
            Direction::Down => crate::tree::Direction::Down,
            Direction::Left => crate::tree::Direction::Left,
            Direction::Right => crate::tree::Direction::Right,
        }
    }
}

/// Workspace cycling direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cycle {
    Next,
    Previous,
}

/// How much of a divider moves per key press, as a fraction of the split.
pub const RESIZE_STEP: f64 = 0.05;

impl Action {
    pub fn storage_id(self) -> String {
        match self {
            Self::FocusDirection(direction) => format!("focus-{}", direction_name(direction)),
            Self::ActivateWorkspace(index) => format!("workspace-{}", index + 1),
            Self::NewWorkspace => "new-workspace".into(),
            Self::SplitPane => "split-pane".into(),
            Self::ResizeDivider(direction) => format!("resize-{}", direction_name(direction)),
            Self::ClosePane => "close-pane".into(),
            Self::OpenPreferences => "preferences".into(),
            Self::ToggleFullscreen => "fullscreen".into(),
            Self::ToggleZoom => "toggle-zoom".into(),
            Self::CycleWorkspace(Cycle::Next) => "next-workspace".into(),
            Self::CycleWorkspace(Cycle::Previous) => "previous-workspace".into(),
        }
    }

    pub fn from_storage_id(id: &str) -> Option<Self> {
        Bindings::default_actions()
            .into_iter()
            .find(|action| action.storage_id() == id)
            .or_else(|| Self::from_legacy_storage_id(id))
    }

    /// Ids written by versions that had a two-valued grow/shrink action.
    ///
    /// Each is read as the arrow it was bound to by default (`Right` grew,
    /// `Left` shrank), so a preferences file from such a version keeps the
    /// user's chosen keys working.
    fn from_legacy_storage_id(id: &str) -> Option<Self> {
        match id {
            "grow-pane" => Some(Self::ResizeDivider(Direction::Right)),
            "shrink-pane" => Some(Self::ResizeDivider(Direction::Left)),
            _ => None,
        }
    }

    /// A stable, human-readable name.
    ///
    /// Shown when an action is invoked with no binding, so it must identify
    /// the action rather than describe the keys.
    pub fn label(self) -> String {
        match self {
            Action::FocusDirection(direction) => format!("Focus {}", direction_name(direction)),
            Action::ActivateWorkspace(index) => format!("Activate workspace {}", index + 1),
            Action::NewWorkspace => "New workspace".into(),
            Action::SplitPane => "Split pane".into(),
            Action::ResizeDivider(direction) => {
                format!("Move divider {}", direction_name(direction))
            }
            Action::ClosePane => "Close pane".into(),
            Action::OpenPreferences => "Open preferences".into(),
            Action::ToggleFullscreen => "Toggle fullscreen".into(),
            Action::ToggleZoom => "Toggle pane zoom".into(),
            Action::CycleWorkspace(Cycle::Next) => "Next workspace".into(),
            Action::CycleWorkspace(Cycle::Previous) => "Previous workspace".into(),
        }
    }
}

fn direction_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Up => "up",
        Direction::Down => "down",
        Direction::Left => "left",
        Direction::Right => "right",
    }
}

/// How many workspaces get direct digit bindings.
///
/// Nine is the practical ceiling: `Alt+0` would be the tenth and reads as a
/// position rather than a digit.
pub const DIRECT_WORKSPACE_BINDINGS: usize = 9;

/// The bindings in force, as accelerator strings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bindings {
    map: BTreeMap<Action, String>,
}

impl Bindings {
    pub fn changes_from(
        &self,
        previous: &Self,
    ) -> std::collections::BTreeMap<String, serde_json::Value> {
        Self::default_actions()
            .into_iter()
            .filter_map(|action| {
                let current = self.accel_for(action);
                (current != previous.accel_for(action))
                    .then(|| (action.storage_id(), serde_json::json!(current)))
            })
            .collect()
    }

    pub fn restore_overrides(
        &mut self,
        overrides: &serde_json::Map<String, serde_json::Value>,
        validate: impl Fn(&str) -> bool,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        let mut accepted = Vec::new();
        // A file can hold both a legacy id and the id that replaced it, for
        // instance after an older version edited a file this version wrote.
        // The current id is the newer decision, so the legacy one yields.
        let current: std::collections::BTreeSet<Action> = overrides
            .keys()
            .filter_map(|id| {
                Action::from_storage_id(id).filter(|action| action.storage_id() == *id)
            })
            .collect();
        for (id, value) in overrides {
            let Some(action) = Action::from_storage_id(id) else {
                warnings.push(format!("Unknown shortcut action: {id}"));
                continue;
            };
            if action.storage_id() != *id && current.contains(&action) {
                continue;
            }
            if value.is_null() {
                self.unbind(action);
            } else if let Some(accel) = value
                .as_str()
                .filter(|accel| accel_is_valid(accel) && validate(accel))
            {
                accepted.push((action, accel.to_string()));
                self.unbind(action);
            } else {
                warnings.push(format!("Invalid shortcut for {id}"));
            }
        }
        let mut owners = std::collections::BTreeMap::new();
        for (action, accel) in accepted {
            if let Some(owner) = owners.insert(accel.clone(), action) {
                warnings.push(format!(
                    "Shortcut conflict: {} displaced {}",
                    action.storage_id(),
                    owner.storage_id()
                ));
            }
            self.rebind(action, &accel);
        }
        warnings
    }

    /// The bindings the application starts with.
    ///
    /// Verified against the GNOME settings schema: `Alt`+arrow, `Alt`+digit,
    /// and `Ctrl+Shift+T/N/W` are claimed by neither the desktop window manager
    /// nor readline or zsh.
    pub fn defaults() -> Self {
        let mut map = BTreeMap::new();
        for direction in [
            Direction::Up,
            Direction::Down,
            Direction::Left,
            Direction::Right,
        ] {
            let key = match direction {
                Direction::Up => "Up",
                Direction::Down => "Down",
                Direction::Left => "Left",
                Direction::Right => "Right",
            };
            map.insert(Action::FocusDirection(direction), format!("<Alt>{key}"));
        }
        for index in 0..DIRECT_WORKSPACE_BINDINGS {
            map.insert(
                Action::ActivateWorkspace(index),
                format!("<Alt>{}", index + 1),
            );
        }
        // The GNOME-standard shortcut for preferences, and unclaimed by the
        // window manager here because the application is already focused when
        // it is pressed.
        map.insert(Action::OpenPreferences, "<Control>comma".into());
        map.insert(Action::ToggleFullscreen, "F11".into());
        map.insert(Action::ToggleZoom, "<Control><Shift>x".into());
        map.insert(Action::NewWorkspace, "<Control><Shift>t".into());
        map.insert(Action::SplitPane, "<Control><Shift>n".into());
        map.insert(Action::ClosePane, "<Control><Shift>w".into());
        // Divider resizing needs its own modifier level from focus movement, so
        // the two cannot be confused by touch. Ctrl+Shift+Arrow is unclaimed by
        // GNOME and matches what Terminator uses for the same job.
        for (direction, key) in [
            (Direction::Up, "Up"),
            (Direction::Down, "Down"),
            (Direction::Left, "Left"),
            (Direction::Right, "Right"),
        ] {
            map.insert(
                Action::ResizeDivider(direction),
                format!("<Control><Shift>{key}"),
            );
        }
        map.insert(
            Action::CycleWorkspace(Cycle::Next),
            "<Control>Page_Down".into(),
        );
        map.insert(
            Action::CycleWorkspace(Cycle::Previous),
            "<Control>Page_Up".into(),
        );
        Self { map }
    }

    /// The sequence bound to an action, if any.
    pub fn accel_for(&self, action: Action) -> Option<&str> {
        self.map.get(&action).map(String::as_str)
    }

    /// The action bound to a sequence, if any.
    #[cfg(test)]
    pub fn action_for(&self, accel: &str) -> Option<Action> {
        self.map
            .iter()
            .find(|(_, bound)| bound.as_str() == accel)
            .map(|(action, _)| *action)
    }

    /// Bind an action to a sequence, replacing any previous binding.
    ///
    /// A sequence already used by another action is taken over, so the mapping
    /// from sequence to action stays a function.
    pub fn rebind(&mut self, action: Action, accel: &str) {
        self.map.retain(|_, bound| bound.as_str() != accel);
        self.map.insert(action, accel.to_string());
    }

    /// Remove an action's binding.
    pub fn unbind(&mut self, action: Action) {
        self.map.remove(&action);
    }

    /// Every action and its current binding, in a stable order.
    pub fn listing(&self) -> Vec<(Action, String)> {
        self.map
            .iter()
            .map(|(action, accel)| (*action, accel.clone()))
            .collect()
    }

    /// Whether an action has a binding.
    pub fn is_bound(&self, action: Action) -> bool {
        self.map.contains_key(&action)
    }

    /// Every action the application can perform, in a stable order.
    ///
    /// Includes unbound actions, because a user needs to see what can be bound
    /// as well as what is bound.
    pub fn default_actions() -> Vec<Action> {
        let mut actions: Vec<Action> = Self::defaults().map.keys().copied().collect();
        actions.sort();
        actions
    }
}

/// Parse an action by its command-line name.
///
/// Used to invoke an action without a key press, which is also how an unbound
/// action becomes reportable rather than silently unreachable.
pub fn parse_action(name: &str) -> Option<Action> {
    let normalised = name.trim().to_ascii_lowercase().replace('-', "_");
    Some(match normalised.as_str() {
        "new_workspace" => Action::NewWorkspace,
        "preferences" | "open_preferences" => Action::OpenPreferences,
        "fullscreen" | "toggle_fullscreen" => Action::ToggleFullscreen,
        "zoom" | "toggle_zoom" => Action::ToggleZoom,
        "resize_up" => Action::ResizeDivider(Direction::Up),
        "resize_down" => Action::ResizeDivider(Direction::Down),
        "resize_left" => Action::ResizeDivider(Direction::Left),
        "resize_right" => Action::ResizeDivider(Direction::Right),
        // Names from the grow/shrink era, read as the arrows they were bound to.
        "grow_pane" | "resize_grow" => Action::ResizeDivider(Direction::Right),
        "shrink_pane" | "resize_shrink" => Action::ResizeDivider(Direction::Left),
        "split_pane" | "split" => Action::SplitPane,
        "close_pane" | "close" => Action::ClosePane,
        "focus_up" => Action::FocusDirection(Direction::Up),
        "focus_down" => Action::FocusDirection(Direction::Down),
        "focus_left" => Action::FocusDirection(Direction::Left),
        "focus_right" => Action::FocusDirection(Direction::Right),
        "next_workspace" => Action::CycleWorkspace(Cycle::Next),
        "previous_workspace" => Action::CycleWorkspace(Cycle::Previous),
        other => {
            let rest = other.strip_prefix("workspace_")?;
            let index = rest.parse::<usize>().ok()?.checked_sub(1)?;
            Action::ActivateWorkspace(index)
        }
    })
}

/// A stable, unique name for an action, used as its `gio` action name.
///
/// Unique matters: GTK maps accelerator to action by name, so two actions
/// sharing a name would make one of them unreachable.
pub fn action_name(action: Action) -> String {
    match action {
        Action::FocusDirection(direction) => format!("focus-{}", direction_name(direction)),
        Action::ActivateWorkspace(index) => format!("workspace-{}", index + 1),
        Action::NewWorkspace => "new-workspace".into(),
        Action::SplitPane => "split-pane".into(),
        Action::ClosePane => "close-pane".into(),
        Action::ResizeDivider(direction) => format!("resize-{}", direction_name(direction)),
        Action::CycleWorkspace(Cycle::Next) => "next-workspace".into(),
        Action::CycleWorkspace(Cycle::Previous) => "previous-workspace".into(),
        Action::OpenPreferences => "preferences".into(),
        Action::ToggleFullscreen => "fullscreen".into(),
        Action::ToggleZoom => "zoom".into(),
    }
}

/// Describe a binding for display, including the unbound case.
pub fn describe(accel: &str) -> String {
    if accel.is_empty() {
        "Disabled".into()
    } else {
        accel.to_string()
    }
}

/// One parsed binding, ready to hand to GTK.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedBinding {
    pub action: Action,
    pub accel: String,
}

/// The modifiers named by an accelerator string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub super_key: bool,
}

impl Modifiers {
    /// True when no modifier is named at all.
    #[cfg(test)]
    pub fn is_empty(self) -> bool {
        !(self.ctrl || self.shift || self.alt || self.super_key)
    }
}

/// A parsed accelerator: its modifiers and its key.
///
/// Parsing is done here rather than delegated to GTK so that a malformed
/// binding can be detected and reported in tests that run without a display.
/// GTK's own parser is still used to build the live trigger.
pub fn parse_accel(accel: &str) -> Option<(Modifiers, String)> {
    let mut modifiers = Modifiers::default();
    let mut key: Option<String> = None;
    let mut rest = accel.trim();

    while let Some(stripped) = rest.strip_prefix('<') {
        let (name, remainder) = stripped.split_once('>')?;
        match name {
            "Control" | "Ctrl" => modifiers.ctrl = true,
            "Shift" => modifiers.shift = true,
            "Alt" => modifiers.alt = true,
            "Super" | "Meta" => modifiers.super_key = true,
            // A single letter is the key, not a modifier.
            other if other.len() == 1 => {
                if key.is_some() {
                    return None;
                }
                key = Some(other.to_string());
                rest = remainder;
                break;
            }
            _ => return None,
        }
        rest = remainder;
    }

    let key = match key {
        Some(key) => key,
        None => {
            let candidate = rest.trim();
            if candidate.is_empty() {
                return None;
            }
            candidate.to_string()
        }
    };

    // A lone modifier with no key is not a shortcut, and unbalanced angle
    // brackets mean the string was never a well-formed accelerator.
    if key.is_empty() || key.contains(['<', '>']) {
        return None;
    }
    Some((modifiers, key))
}

/// Whether an accelerator names a key the application can bind.
pub fn accel_is_valid(accel: &str) -> bool {
    parse_accel(accel).is_some()
}

/// Whether an accelerator would be swallowed by the desktop environment.
pub fn is_reserved_by_desktop(accel: &str) -> bool {
    let Some((modifiers, _)) = parse_accel(accel) else {
        return false;
    };
    // GNOME claims Super sequences for overview and workspace switching, and
    // Ctrl+Alt+Arrow for its own workspaces. Binding any of these would mean
    // the action silently never fires.
    modifiers.super_key || (modifiers.ctrl && modifiers.alt)
}

/// Bindings that cannot be turned into a shortcut.
///
/// Checked without a display so the registry is verifiable in a headless test
/// run; the live trigger is built separately by [`resolve`].
pub fn invalid_bindings(bindings: &Bindings) -> Vec<(Action, String)> {
    bindings
        .listing()
        .into_iter()
        .filter(|(_, accel)| !accel_is_valid(accel))
        .collect()
}

/// Default bindings the desktop environment would swallow.
///
/// These are reported rather than silently accepted, because an action bound
/// to a reserved sequence never fires and looks like a broken application.
pub fn reserved_bindings(bindings: &Bindings) -> Vec<(Action, String)> {
    bindings
        .listing()
        .into_iter()
        .filter(|(_, accel)| is_reserved_by_desktop(accel))
        .collect()
}

/// Build the GTK trigger for every binding.
///
/// Requires GTK to be initialised: `ShortcutTrigger::parse_string` asserts on
/// it, so calling this from a headless test would abort the process. Use
/// [`invalid_bindings`] where a display is not available.
pub fn resolve(bindings: &Bindings) -> Vec<(ResolvedBinding, Option<gtk::ShortcutTrigger>)> {
    if !gtk::is_initialized() {
        return Vec::new();
    }
    bindings
        .listing()
        .into_iter()
        .map(|(action, accel)| {
            let trigger = gtk::ShortcutTrigger::parse_string(&accel);
            (ResolvedBinding { action, accel }, trigger)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn preference_ids_and_override_changes_round_trip() {
        use super::*;
        for action in Bindings::default_actions() {
            assert_eq!(Action::from_storage_id(&action.storage_id()), Some(action));
        }
        assert_eq!(Action::from_storage_id("unknown"), None);
        let defaults = Bindings::defaults();
        assert!(defaults.changes_from(&defaults).is_empty());
        let mut edited = defaults.clone();
        edited.rebind(Action::SplitPane, "F11");
        edited.unbind(Action::ToggleZoom);
        let changes = edited.changes_from(&defaults);
        assert_eq!(changes["fullscreen"], serde_json::Value::Null);
        assert_eq!(changes["toggle-zoom"], serde_json::Value::Null);
        let overrides = changes.into_iter().collect();
        let mut restored = Bindings::defaults();
        assert!(restored
            .restore_overrides(&overrides, accel_is_valid)
            .is_empty());
        assert_eq!(restored, edited);
    }

    #[test]
    fn stored_invalid_and_conflicting_overrides_are_reported() {
        use super::*;
        let mut bindings = Bindings::defaults();
        let value = serde_json::json!({"unknown":"F8","split-pane":"F11","new-workspace":"F11","toggle-zoom":42});
        let warnings = bindings.restore_overrides(value.as_object().unwrap(), accel_is_valid);
        assert_eq!(warnings.len(), 3);
        assert_eq!(bindings.accel_for(Action::SplitPane), Some("F11"));
        assert_eq!(bindings.accel_for(Action::NewWorkspace), None);
        assert_eq!(bindings.accel_for(Action::ToggleFullscreen), None);
        assert_eq!(
            bindings.accel_for(Action::ToggleZoom),
            Some("<Control><Shift>x")
        );
    }
    use super::*;

    #[test]
    fn defaults_bind_the_five_core_actions() {
        let bindings = Bindings::defaults();
        assert_eq!(
            bindings.accel_for(Action::NewWorkspace),
            Some("<Control><Shift>t")
        );
        assert_eq!(
            bindings.accel_for(Action::SplitPane),
            Some("<Control><Shift>n")
        );
        assert_eq!(
            bindings.accel_for(Action::ClosePane),
            Some("<Control><Shift>w")
        );
        assert_eq!(
            bindings.accel_for(Action::FocusDirection(Direction::Left)),
            Some("<Alt>Left")
        );
        assert_eq!(
            bindings.accel_for(Action::ActivateWorkspace(0)),
            Some("<Alt>1")
        );
    }

    #[test]
    fn the_focus_directions_are_all_bound() {
        let bindings = Bindings::defaults();
        for direction in [
            Direction::Up,
            Direction::Down,
            Direction::Left,
            Direction::Right,
        ] {
            assert!(bindings.is_bound(Action::FocusDirection(direction)));
        }
    }

    #[test]
    fn workspace_digits_cover_one_through_nine() {
        let bindings = Bindings::defaults();
        assert_eq!(
            bindings.accel_for(Action::ActivateWorkspace(8)),
            Some("<Alt>9")
        );
        assert!(!bindings.is_bound(Action::ActivateWorkspace(9)));
    }

    #[test]
    fn keyboard_resize_is_bound_to_a_distinct_modifier_level() {
        let bindings = Bindings::defaults();
        let resize = bindings
            .accel_for(Action::ResizeDivider(Direction::Right))
            .unwrap();
        let focus = bindings
            .accel_for(Action::FocusDirection(Direction::Right))
            .unwrap();
        assert_eq!(resize, "<Control><Shift>Right");
        assert_ne!(resize, focus, "resize and focus must not share a sequence");
    }

    #[test]
    fn every_ctrl_shift_arrow_is_bound_to_its_own_resize_action() {
        // The defaults once keyed all four arrows by a two-valued grow/shrink
        // action, so Left/Right overwrote Up/Down and those two keys did
        // nothing at all.
        let bindings = Bindings::defaults();
        let actions: Vec<Option<Action>> = ["Up", "Down", "Left", "Right"]
            .iter()
            .map(|key| bindings.action_for(&format!("<Control><Shift>{key}")))
            .collect();
        assert!(
            actions.iter().all(Option::is_some),
            "an arrow is unbound: {actions:?}"
        );
        let distinct: std::collections::BTreeSet<_> = actions.iter().flatten().collect();
        assert_eq!(distinct.len(), 4, "arrows share an action: {actions:?}");
    }

    #[test]
    fn workspace_cycling_is_bound_separately_from_direct_selection() {
        let bindings = Bindings::defaults();
        assert_eq!(
            bindings.accel_for(Action::CycleWorkspace(Cycle::Next)),
            Some("<Control>Page_Down")
        );
        assert_eq!(
            bindings.accel_for(Action::CycleWorkspace(Cycle::Previous)),
            Some("<Control>Page_Up")
        );
    }

    #[test]
    fn every_default_binding_is_unique() {
        // A shared sequence would make one of the two actions unreachable.
        let listing = Bindings::defaults().listing();
        for (index, (_, accel)) in listing.iter().enumerate() {
            assert!(
                !listing[index + 1..].iter().any(|(_, other)| other == accel),
                "{accel} is bound twice"
            );
        }
    }

    #[test]
    fn resize_actions_parse_from_their_names() {
        for (name, direction) in [
            ("resize-up", Direction::Up),
            ("resize-down", Direction::Down),
            ("resize-left", Direction::Left),
            ("resize-right", Direction::Right),
            // Earlier names keep working as the arrows they were bound to.
            ("grow-pane", Direction::Right),
            ("shrink-pane", Direction::Left),
        ] {
            assert_eq!(
                parse_action(name),
                Some(Action::ResizeDivider(direction)),
                "{name}"
            );
        }
    }

    #[test]
    fn a_stored_grow_or_shrink_override_still_applies() {
        // A preferences file written before the four-direction actions keyed
        // these overrides by grow/shrink; dropping them would silently lose
        // the user's chosen keys.
        let mut bindings = Bindings::defaults();
        let overrides = serde_json::json!({
            "grow-pane": "<Control><Alt>l",
            "shrink-pane": "<Control><Alt>h",
        });
        let warnings = bindings.restore_overrides(overrides.as_object().unwrap(), |_| true);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            bindings.accel_for(Action::ResizeDivider(Direction::Right)),
            Some("<Control><Alt>l")
        );
        assert_eq!(
            bindings.accel_for(Action::ResizeDivider(Direction::Left)),
            Some("<Control><Alt>h")
        );
    }

    #[test]
    fn a_current_resize_id_wins_over_its_legacy_id() {
        let mut bindings = Bindings::defaults();
        let overrides = serde_json::json!({
            "grow-pane": "<Control><Alt>l",
            "resize-right": "<Control><Alt>r",
        });
        bindings.restore_overrides(overrides.as_object().unwrap(), |_| true);
        assert_eq!(
            bindings.accel_for(Action::ResizeDivider(Direction::Right)),
            Some("<Control><Alt>r")
        );
    }

    #[test]
    fn no_default_binding_uses_the_super_key() {
        for (_, accel) in Bindings::defaults().listing() {
            assert!(!accel.contains("Super"), "{accel} uses Super");
        }
    }

    #[test]
    fn no_default_binding_shadows_desktop_workspace_switching() {
        // GNOME reserves Ctrl+Alt+Arrow for its own workspace switching.
        for (_, accel) in Bindings::defaults().listing() {
            assert!(
                !accel.contains("<Control><Alt>"),
                "{accel} collides with the desktop"
            );
        }
    }

    #[test]
    fn bindings_can_be_listed() {
        let listing = Bindings::defaults().listing();
        assert!(!listing.is_empty());
        // Listing twice yields the same order.
        assert_eq!(listing, Bindings::defaults().listing());
    }

    #[test]
    fn lookup_works_in_both_directions() {
        let bindings = Bindings::defaults();
        let accel = bindings.accel_for(Action::SplitPane).unwrap().to_string();
        assert_eq!(bindings.action_for(&accel), Some(Action::SplitPane));
    }

    #[test]
    fn rebinding_replaces_the_previous_sequence() {
        let mut bindings = Bindings::defaults();
        let old = bindings.accel_for(Action::SplitPane).unwrap().to_string();

        bindings.rebind(Action::SplitPane, "<Control><Shift>d");
        assert_eq!(
            bindings.accel_for(Action::SplitPane),
            Some("<Control><Shift>d")
        );
        assert_eq!(bindings.action_for(&old), None, "old sequence still bound");
    }

    #[test]
    fn rebinding_onto_a_taken_sequence_transfers_it() {
        let mut bindings = Bindings::defaults();
        bindings.rebind(Action::SplitPane, "<Control><Shift>t");
        assert_eq!(
            bindings.accel_for(Action::SplitPane),
            Some("<Control><Shift>t")
        );
        assert_eq!(bindings.accel_for(Action::NewWorkspace), None);
        assert_eq!(
            bindings.action_for("<Control><Shift>t"),
            Some(Action::SplitPane)
        );
    }

    #[test]
    fn unbinding_leaves_the_action_reported_as_unbound() {
        let mut bindings = Bindings::defaults();
        bindings.unbind(Action::SplitPane);
        assert!(!bindings.is_bound(Action::SplitPane));
        assert_eq!(bindings.action_for("<Control><Shift>n"), None);
    }

    #[test]
    fn action_labels_identify_the_action() {
        assert_eq!(Action::SplitPane.label(), "Split pane");
        assert_eq!(Action::ClosePane.label(), "Close pane");
        assert_eq!(Action::FocusDirection(Direction::Up).label(), "Focus up");
        // Displayed positions are 1-based even though they are 0-based here.
        assert_eq!(Action::ActivateWorkspace(0).label(), "Activate workspace 1");
    }

    #[test]
    fn every_default_binding_is_well_formed() {
        assert!(invalid_bindings(&Bindings::defaults()).is_empty());
    }

    #[test]
    fn an_unparsable_binding_is_reported_rather_than_silently_dropped() {
        let mut bindings = Bindings::defaults();
        bindings.rebind(Action::SplitPane, "<<broken");
        let invalid = invalid_bindings(&bindings);
        assert_eq!(invalid.len(), 1);
        assert_eq!(invalid[0].0, Action::SplitPane);
    }

    #[test]
    fn no_default_binding_would_be_swallowed_by_the_desktop() {
        assert!(reserved_bindings(&Bindings::defaults()).is_empty());
    }

    #[test]
    fn a_binding_the_desktop_owns_is_reported() {
        let mut bindings = Bindings::defaults();
        bindings.rebind(Action::SplitPane, "<Control><Alt>n");
        bindings.rebind(Action::NewWorkspace, "<Super>t");
        let reserved = reserved_bindings(&bindings);
        assert_eq!(reserved.len(), 2);
    }

    #[test]
    fn accelerators_parse_into_modifiers_and_a_key() {
        let (modifiers, key) = parse_accel("<Control><Shift>n").unwrap();
        assert!(modifiers.ctrl && modifiers.shift && !modifiers.alt);
        assert_eq!(key, "n");

        let (modifiers, key) = parse_accel("<Alt>Up").unwrap();
        assert!(modifiers.alt && !modifiers.is_empty());
        assert_eq!(key, "Up");

        let (modifiers, key) = parse_accel("F5").unwrap();
        assert!(modifiers.is_empty());
        assert_eq!(key, "F5");
    }

    #[test]
    fn a_single_letter_is_treated_as_the_key_not_a_modifier() {
        let (modifiers, key) = parse_accel("<Control>a").unwrap();
        assert!(modifiers.ctrl);
        assert_eq!(key, "a");
    }

    #[test]
    fn malformed_accelerators_are_rejected() {
        for bad in ["<<broken", "<Control", "Control>", "", "   ", "<Nope>x"] {
            assert!(!accel_is_valid(bad), "{bad} should be rejected");
        }
    }

    #[test]
    fn resolve_yields_nothing_without_a_display() {
        // Guards the headless path: building triggers would abort the process.
        if !gtk::is_initialized() {
            assert!(resolve(&Bindings::defaults()).is_empty());
        }
    }

    #[test]
    fn actions_parse_from_their_names() {
        assert_eq!(parse_action("split-pane"), Some(Action::SplitPane));
        assert_eq!(parse_action("close"), Some(Action::ClosePane));
        assert_eq!(parse_action("new-workspace"), Some(Action::NewWorkspace));
        assert_eq!(
            parse_action("focus-right"),
            Some(Action::FocusDirection(Direction::Right))
        );
        assert_eq!(
            parse_action("workspace-1"),
            Some(Action::ActivateWorkspace(0))
        );
    }

    #[test]
    fn unknown_action_names_are_rejected() {
        assert_eq!(parse_action("fly-away"), None);
        assert_eq!(parse_action(""), None);
        // Position 0 has no one-based spelling, so it cannot be named.
        assert_eq!(parse_action("workspace-0"), None);
    }

    #[test]
    fn every_default_action_has_a_unique_name() {
        let names: Vec<String> = Bindings::default_actions()
            .into_iter()
            .map(action_name)
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            names.len(),
            sorted.len(),
            "duplicate action names in {names:?}"
        );
    }

    #[test]
    fn action_names_are_usable_as_gio_names() {
        for action in Bindings::default_actions() {
            let name = action_name(action);
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{name} is not a valid action name"
            );
        }
    }

    #[test]
    fn the_requested_shortcuts_are_bound() {
        let bindings = Bindings::defaults();
        assert_eq!(bindings.accel_for(Action::ToggleFullscreen), Some("F11"));
        assert_eq!(
            bindings.accel_for(Action::ToggleZoom),
            Some("<Control><Shift>x")
        );
    }

    #[test]
    fn directions_convert_to_tree_directions() {
        assert_eq!(
            crate::tree::Direction::from(Direction::Left),
            crate::tree::Direction::Left
        );
        assert_eq!(
            crate::tree::Direction::from(Direction::Down),
            crate::tree::Direction::Down
        );
    }
}
