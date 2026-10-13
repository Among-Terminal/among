//! native VTE menu routing keeps mouse-reporting applications in control of
//! unmodified clicks; only the application supplies the menu contents.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use vte::prelude::*;

use crate::tree::PaneId;

#[derive(Clone, Debug)]
pub enum Command {
    Split,
    Zoom,
    Close,
    OpenLink(String),
}

pub struct Presentation {
    pub zoomed: bool,
    pub split_accel: Option<String>,
    pub zoom_accel: Option<String>,
    pub close_accel: Option<String>,
}

pub struct Callbacks {
    pub prepare: Rc<dyn Fn(PaneId) -> Option<Presentation>>,
    pub valid: Rc<dyn Fn(PaneId) -> bool>,
    pub command: Rc<dyn Fn(PaneId, Command)>,
    pub dismiss: Rc<dyn Fn(PaneId)>,
}

struct Inner {
    id: PaneId,
    terminal: glib::WeakRef<vte::Terminal>,
    popover: gtk::PopoverMenu,
    callbacks: Callbacks,
    generation: Cell<u64>,
    open: Cell<bool>,
    actions: RefCell<Option<gio::SimpleActionGroup>>,
}

pub struct ContextMenu {
    inner: Rc<Inner>,
    setup: Option<glib::SignalHandlerId>,
    selection: Option<glib::SignalHandlerId>,
    clipboard: gtk::gdk::Clipboard,
    clipboard_changed: Option<glib::SignalHandlerId>,
}

impl ContextMenu {
    pub fn new(id: PaneId, terminal: &vte::Terminal, callbacks: Callbacks) -> Self {
        let popover = gtk::PopoverMenu::from_model(None::<&gio::Menu>);
        terminal.set_context_menu(Some(&popover));
        terminal.set_allow_hyperlink(true);
        // VTE requires PCRE2_MULTILINE for terminal match expressions.
        match vte::Regex::for_match(PLAIN_URI_PATTERN, 0x400) {
            Ok(regex) => {
                terminal.match_add_regex(&regex, 0);
            }
            Err(error) => eprintln!("among: link matcher unavailable: {error}"),
        }
        let inner = Rc::new(Inner {
            id,
            terminal: terminal.downgrade(),
            popover,
            callbacks,
            generation: Cell::new(0),
            open: Cell::new(false),
            actions: RefCell::new(None),
        });
        let weak = Rc::downgrade(&inner);
        let setup = connect_setup(terminal, move |location| {
            if let Some(inner) = weak.upgrade() {
                match location {
                    Setup::Open(coords) => inner.prepare(coords),
                    Setup::Dismiss => {
                        // GTK may dismiss the popover before dispatching its action
                        // in this same turn. Explicit rebuild invalidation is immediate.
                        let generation = inner.generation.get();
                        let weak = Rc::downgrade(&inner);
                        glib::idle_add_local_once(move || {
                            if let Some(inner) = weak.upgrade() {
                                if inner.generation.get() == generation {
                                    inner.open.set(false);
                                    inner.generation.set(generation.wrapping_add(1));
                                    (inner.callbacks.dismiss)(inner.id);
                                }
                            }
                        });
                    }
                }
            }
        });
        let weak = Rc::downgrade(&inner);
        let selection = terminal.connect_selection_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.refresh_clipboard();
            }
        });
        let clipboard = terminal.clipboard();
        let weak = Rc::downgrade(&inner);
        let clipboard_changed = clipboard.connect_changed(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.refresh_clipboard();
            }
        });
        Self {
            inner,
            setup: Some(setup),
            selection: Some(selection),
            clipboard,
            clipboard_changed: Some(clipboard_changed),
        }
    }

    pub fn invalidate(&self) {
        self.inner.open.set(false);
        self.inner
            .generation
            .set(self.inner.generation.get().wrapping_add(1));
        self.inner.popover.popdown();
    }

    pub fn is_open(&self) -> bool {
        self.inner.open.get()
    }

    pub fn inspection(&self) -> Vec<String> {
        let Some(root) = self
            .inner
            .popover
            .root()
            .and_then(|root| root.dynamic_cast::<gtk::Widget>().ok())
        else {
            return Vec::new();
        };
        let mut result = Vec::new();
        let surface_offset = self.inner.popover.surface().and_then(|surface| {
            let popup = surface.downcast::<gtk::gdk::Popup>().ok()?;
            let (px, py) = self.inner.popover.surface_transform();
            let (rx, ry) = root.native()?.surface_transform();
            Some((
                popup.position_x() as f32 - px as f32 + rx as f32,
                popup.position_y() as f32 - py as f32 + ry as f32,
            ))
        });
        fn visit(
            widget: &gtk::Widget,
            root: &gtk::Widget,
            popover: &gtk::Widget,
            surface_offset: Option<(f32, f32)>,
            result: &mut Vec<String>,
        ) {
            if let Some(image) = widget.downcast_ref::<gtk::Image>() {
                if image.is_mapped() && image.width() > 0 && image.height() > 0 {
                    if let Some(name) =
                        image.icon_name().map(|name| name.to_string()).or_else(|| {
                            image
                                .gicon()
                                .and_then(|icon| icon.to_string())
                                .map(|s| s.to_string())
                        })
                    {
                        result.push(format!("icon:{name}:{}x{}", image.width(), image.height()));
                    }
                }
            }
            if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                // GTK 4.14 cannot always compute across a native popup surface.
                // The selfcheck driver still needs real window-relative bounds.
                let bounds = widget.compute_bounds(root).or_else(|| {
                    let (x, y) = surface_offset?;
                    let rect = widget.compute_bounds(popover)?;
                    Some(gtk::graphene::Rect::new(
                        rect.x() + x,
                        rect.y() + y,
                        rect.width(),
                        rect.height(),
                    ))
                });
                if let Some(rect) = bounds {
                    result.push(format!(
                        "{}:{}:{:.0}+{:.0}+{:.0}x{:.0}",
                        label.text(),
                        widget.is_sensitive(),
                        rect.x(),
                        rect.y(),
                        rect.width(),
                        rect.height()
                    ));
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                visit(&widget, root, popover, surface_offset, result);
                child = widget.next_sibling();
            }
        }
        if self.is_open() {
            visit(
                self.inner.popover.upcast_ref(),
                &root,
                self.inner.popover.upcast_ref(),
                surface_offset,
                &mut result,
            );
        }
        result
    }
}

impl Drop for ContextMenu {
    fn drop(&mut self) {
        self.invalidate();
        if let Some(terminal) = self.inner.terminal.upgrade() {
            if let Some(id) = self.setup.take() {
                terminal.disconnect(id);
            }
            if let Some(id) = self.selection.take() {
                terminal.disconnect(id);
            }
            terminal.insert_action_group("terminal-menu", None::<&gio::SimpleActionGroup>);
            // VTE owns the menu until terminal finalization. In VTE 0.76,
            // replacing it with NULL passes NULL to signal disconnection
            // (Widget::set_context_menu); leave its owned reference intact.
            // Every application callback below is disconnected or weak.
        }
        if let Some(id) = self.clipboard_changed.take() {
            self.clipboard.disconnect(id);
        }
    }
}

impl Inner {
    fn prepare(self: &Rc<Self>, coordinates: Option<(f64, f64)>) {
        let Some(presentation) = (self.callbacks.prepare)(self.id) else {
            return;
        };
        let Some(terminal) = self.terminal.upgrade() else {
            return;
        };
        let link = coordinates.and_then(|(x, y)| {
            if let Some(uri) = terminal.check_hyperlink_at(x, y) {
                return Link::parse(uri.as_str());
            }
            terminal.check_match_at(x, y).0.and_then(|uri| {
                // Trim sentence punctuation only from a plain-text match, never OSC 8.
                Link::parse(uri.trim_end_matches(['.', ',', ';', ':', '!', '?']))
            })
        });
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.open.set(true);
        let group = gio::SimpleActionGroup::new();
        for name in [
            "copy",
            "paste",
            "select-all",
            "open-link",
            "copy-link",
            "split",
            "zoom",
            "close",
        ] {
            let action = gio::SimpleAction::new(name, None);
            if name == "open-link" {
                action.set_enabled(link.as_ref().is_some_and(|link| link.openable));
            }
            if name == "copy-link" {
                action.set_enabled(link.is_some());
            }
            let weak = Rc::downgrade(self);
            let link = link.clone();
            action.connect_activate(move |_, _| {
                let Some(inner) = weak.upgrade() else { return };
                if generation != inner.generation.get() || !(inner.callbacks.valid)(inner.id) {
                    return;
                }
                let Some(terminal) = inner.terminal.upgrade() else {
                    return;
                };
                match name {
                    "copy" => terminal.copy_clipboard_format(vte::Format::Text),
                    "paste" => terminal.paste_clipboard(),
                    "select-all" => terminal.select_all(),
                    "copy-link" => {
                        if let Some(link) = &link {
                            terminal.clipboard().set_text(&link.uri);
                        }
                    }
                    "open-link" => {
                        if let Some(link) = &link {
                            if link.openable {
                                inner.dispatch(generation, Command::OpenLink(link.uri.clone()));
                            }
                        }
                    }
                    "split" => inner.dispatch(generation, Command::Split),
                    "zoom" => inner.dispatch(generation, Command::Zoom),
                    "close" => inner.dispatch(generation, Command::Close),
                    _ => {}
                }
            });
            group.add_action(&action);
        }
        terminal.insert_action_group("terminal-menu", Some(&group));
        *self.actions.borrow_mut() = Some(group);
        self.refresh_clipboard();
        let menu = gio::Menu::new();
        let clipboard = gio::Menu::new();
        item(&clipboard, "Copy", "copy", None);
        item(&clipboard, "Paste", "paste", None);
        item(&clipboard, "Select All", "select-all", None);
        menu.append_section(None, &clipboard);
        if link.is_some() {
            let links = gio::Menu::new();
            item(&links, "Open Link", "open-link", None);
            item(&links, "Copy Link", "copy-link", None);
            menu.append_section(None, &links);
        }
        let panes = gio::Menu::new();
        item(
            &panes,
            "Split Pane",
            "split",
            presentation.split_accel.as_deref(),
        );
        item(
            &panes,
            if presentation.zoomed {
                "Restore Pane"
            } else {
                "Expand Pane"
            },
            "zoom",
            presentation.zoom_accel.as_deref(),
        );
        item(
            &panes,
            "Close Terminal",
            "close",
            presentation.close_accel.as_deref(),
        );
        menu.append_section(None, &panes);
        self.popover.set_menu_model(Some(&menu));
        install_rows(&self.popover, &menu);
    }

    fn dispatch(self: &Rc<Self>, generation: u64, command: Command) {
        // Reparenting or closing here would destroy the emitting GTK button.
        // Recheck the target after its activation has unwound.
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(inner) = weak.upgrade() {
                if inner.generation.get() == generation && (inner.callbacks.valid)(inner.id) {
                    (inner.callbacks.command)(inner.id, command);
                }
            }
        });
    }

    fn refresh_clipboard(&self) {
        let Some(terminal) = self.terminal.upgrade() else {
            return;
        };
        let group = self.actions.borrow();
        let Some(group) = group.as_ref() else { return };
        for (name, enabled) in [
            ("copy", terminal.has_selection()),
            (
                "paste",
                terminal
                    .clipboard()
                    .formats()
                    .contains_type(String::static_type()),
            ),
        ] {
            if let Some(action) = group
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                action.set_enabled(enabled);
            }
        }
    }
}

fn item(menu: &gio::Menu, label: &str, action: &str, accelerator: Option<&str>) {
    let item = gio::MenuItem::new(Some(label), Some(&format!("terminal-menu.{action}")));
    let icon = match action {
        "copy" => "edit-copy-symbolic",
        "paste" => "edit-paste-symbolic",
        "select-all" => "edit-select-all-symbolic",
        "open-link" => "web-browser-symbolic",
        "copy-link" => "edit-copy-symbolic",
        "split" => "view-dual-symbolic",
        "zoom" if label == "Restore Pane" => "view-restore-symbolic",
        "zoom" => "view-fullscreen-symbolic",
        "close" => "window-close-symbolic",
        _ => "application-x-executable-symbolic",
    };
    item.set_icon(&gio::ThemedIcon::new(icon));
    item.set_attribute_value("custom", Some(&action.to_variant()));
    if let Some(accelerator) = accelerator {
        item.set_attribute_value("accel", Some(&accelerator.to_variant()));
    }
    menu.append_item(&item);
}

fn install_rows(popover: &gtk::PopoverMenu, menu: &gio::Menu) {
    for section in 0..menu.n_items() {
        let Some(section) = menu.item_link(section, "section") else {
            continue;
        };
        for index in 0..section.n_items() {
            let attribute = |name| section.item_attribute_value(index, name, None);
            let Some(id) = attribute("custom").and_then(|v| v.get::<String>()) else {
                continue;
            };
            let Some(label) = attribute("label").and_then(|v| v.get::<String>()) else {
                continue;
            };
            let button = gtk::Button::new();
            button.add_css_class("flat");
            button.set_hexpand(true);
            button.update_property(&[gtk::accessible::Property::Label(&label)]);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            if let Some(icon) = attribute("icon").and_then(|v| gio::Icon::deserialize(&v)) {
                row.append(&gtk::Image::from_gicon(&icon));
            }
            let text = gtk::Label::new(Some(&label));
            text.set_xalign(0.0);
            text.set_hexpand(true);
            row.append(&text);
            if let Some(accel) = attribute("accel").and_then(|v| v.get::<String>()) {
                let display = gtk::accelerator_parse(&accel)
                    .map(|(key, modifiers)| gtk::accelerator_get_label(key, modifiers).to_string())
                    .unwrap_or(accel);
                let hint = gtk::Label::new(Some(&display));
                hint.add_css_class("dim-label");
                row.append(&hint);
            }
            button.set_child(Some(&row));
            if let Some(action) = attribute("action").and_then(|v| v.get::<String>()) {
                button.set_action_name(Some(&action));
            }
            let weak = popover.downgrade();
            button.connect_clicked(move |_| {
                if let Some(popover) = weak.upgrade() {
                    popover.popdown();
                }
            });
            popover.add_child(&button, &id);
        }
    }
}

// A conservative absolute-URI match: paired delimiters and quotes are not guessed.
const PLAIN_URI_PATTERN: &str = r#"(?i)\b(?:https?://|mailto:)[^\s<>"'()\[\]{}]+"#;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Link {
    uri: String,
    openable: bool,
}

impl Link {
    fn parse(value: &str) -> Option<Self> {
        if value.len() > 8192 || value.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return None;
        }
        let uri = glib::Uri::parse(value, glib::UriFlags::NONE).ok()?;
        let scheme = uri.scheme().to_ascii_lowercase();
        if matches!(scheme.as_str(), "http" | "https")
            && uri.host().is_none_or(|host| host.is_empty())
        {
            return None;
        }
        if uri.path().is_empty() && scheme == "mailto" {
            return None;
        }
        Some(Self {
            uri: value.to_owned(),
            openable: matches!(scheme.as_str(), "http" | "https" | "mailto"),
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum Setup {
    Open(Option<(f64, f64)>),
    Dismiss,
}

fn connect_setup<F: Fn(Setup) + 'static>(
    terminal: &vte::Terminal,
    callback: F,
) -> glib::SignalHandlerId {
    use glib::translate::*;
    unsafe extern "C" fn trampoline<F: Fn(Setup) + 'static>(
        _: *mut vte::ffi::VteTerminal,
        context: *const vte::ffi::VteEventContext,
        data: glib::ffi::gpointer,
    ) {
        // VTE lends context only for this signal emission. Extract coordinates
        // now; neither the opaque pointer nor the borrowed callback escapes.
        let callback = unsafe { &*(data as *const F) };
        let setup = if context.is_null() {
            Setup::Dismiss
        } else {
            let (mut x, mut y) = (0.0, 0.0);
            let pointer: bool = unsafe {
                from_glib(vte::ffi::vte_event_context_get_coordinates(
                    context, &mut x, &mut y,
                ))
            };
            Setup::Open(if pointer && x.is_finite() && y.is_finite() {
                Some((x, y))
            } else {
                None
            })
        };
        // Do not let a future callback bug unwind through the C signal boundary.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(setup))).is_err() {
            eprintln!("among: context-menu callback failed");
        }
    }
    // connect_raw installs a destroy notifier for the boxed callback. VTE owns
    // that allocation until disconnect/finalization; this matches its signal ABI.
    unsafe {
        glib::signal::connect_raw(
            terminal.as_ptr().cast(),
            c"setup-context-menu".as_ptr(),
            Some(std::mem::transmute::<*const (), unsafe extern "C" fn()>(
                trampoline::<F> as *const (),
            )),
            Box::into_raw(Box::new(callback)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an isolated graphical session and private clipboard"]
    fn native_menu_revokes_saved_actions_and_refreshes_clipboard() {
        fn pump() {
            let end = std::time::Instant::now() + std::time::Duration::from_millis(400);
            while std::time::Instant::now() < end {
                while glib::MainContext::default().pending() {
                    glib::MainContext::default().iteration(false);
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .expect("never test against personal configuration: XDG_CONFIG_HOME is not set");
        crate::test_isolation::assert_below_check_root(std::path::Path::new(&config));
        gtk::init().expect("isolated GTK display");
        let terminal = vte::Terminal::new();
        let window = gtk::Window::builder()
            .default_width(640)
            .default_height(400)
            .child(&terminal)
            .build();
        let calls = Rc::new(Cell::new(0));
        let valid = Rc::new(Cell::new(true));
        let menu = ContextMenu::new(
            PaneId(1),
            &terminal,
            Callbacks {
                prepare: Rc::new(|_| {
                    Some(Presentation {
                        zoomed: false,
                        split_accel: None,
                        zoom_accel: None,
                        close_accel: None,
                    })
                }),
                valid: Rc::new({
                    let valid = valid.clone();
                    move |_| valid.get()
                }),
                command: Rc::new({
                    let calls = calls.clone();
                    move |_, _| calls.set(calls.get() + 1)
                }),
                dismiss: Rc::new(|_| {}),
            },
        );
        window.present();
        terminal.feed(b"owned synthetic selection\r\n");
        pump();
        let clipboard = terminal.clipboard();
        let binary = gtk::gdk::ContentProvider::for_bytes(
            "application/octet-stream",
            &glib::Bytes::from_static(b"binary"),
        );
        clipboard
            .set_content(Some(&binary))
            .expect("private clipboard");
        menu.inner.prepare(None);
        // VTE parents this popover during native invocation; this widget-level
        // test prepares it directly, so supply the same terminal parent first.
        menu.inner.popover.set_parent(&terminal);
        menu.inner.popover.popup();
        pump();
        assert!(menu.inner.popover.is_mapped());
        let group = menu.inner.actions.borrow().clone().expect("native actions");
        assert!(!group.is_action_enabled("copy"));
        assert!(!group.is_action_enabled("paste"));

        clipboard.set_text("private synthetic clipboard");
        pump();
        assert!(menu.is_open());
        assert!(group.is_action_enabled("paste"));
        group.activate_action("select-all", None);
        pump();
        assert!(terminal.has_selection());
        assert!(group.is_action_enabled("copy"));
        terminal.unselect_all();
        clipboard
            .set_content(Some(&binary))
            .expect("private clipboard");
        pump();
        assert!(menu.is_open());
        assert!(!group.is_action_enabled("copy"));
        assert!(!group.is_action_enabled("paste"));

        // Keep the actual old GAction group, including its activation closures.
        // Both queued dispatch and later reuse must fail after revocation.
        group.activate_action("split", None);
        menu.invalidate();
        pump();
        group.activate_action("close", None);
        pump();
        assert_eq!(calls.get(), 0);
        menu.inner.prepare(None);
        let fresh = menu.inner.actions.borrow().clone().expect("fresh actions");
        fresh.activate_action("zoom", None);
        valid.set(false);
        pump();
        assert_eq!(calls.get(), 0);
        valid.set(true);
        fresh.activate_action("split", None);
        pump();
        assert_eq!(calls.get(), 1, "current actions still dispatch");
        menu.inner.popover.unparent();
        drop(menu);
        fresh.activate_action("close", None);
        pump();
        assert_eq!(calls.get(), 1, "teardown revokes retained actions");
        window.destroy();
        pump();
        println!("PASS real VTE selection and private clipboard refresh an open GTK menu; retained and queued actions are revoked");
    }

    #[test]
    fn uri_policy_never_turns_unsupported_schemes_into_commands() {
        for value in [
            "https://example.test/path?a=1&b=2",
            "HTTP://example.test",
            "mailto:a@example.test",
        ] {
            let link = Link::parse(value).expect("valid supported URI");
            assert!(link.openable);
            assert_eq!(link.uri, value);
        }
        for value in ["file:///tmp/test", "javascript:alert(1)", "custom:test"] {
            assert!(!Link::parse(value).expect("valid copy-only URI").openable);
        }
        for value in [
            "not-a-uri",
            "https:///missing-host",
            "https://a.test/\ncommand",
            "mailto:",
        ] {
            assert!(Link::parse(value).is_none(), "{value:?}");
        }
        assert!(Link::parse(&format!("https://example.test/{}", "a".repeat(8192))).is_none());
    }
}
