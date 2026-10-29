//! The pane widget: a terminal plus the application's own chrome around it.
//!
//! A pane is a container rather than a bare terminal so that each pane owns its
//! own header and footer slot areas. That is what lets plugin content appear
//! next to one pane's terminal without leaking into a neighbour or the
//! workspace as a whole.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gdk;
use gtk::glib::prelude::ToValue;
use gtk::prelude::*;
use vte::prelude::*;

use crate::size::{self, CellSize, TermSize};
use crate::slots::{self, Scope, SlotStore};
use crate::tree::PaneId;

/// How much text a slot shows before eliding.
const PANE_SLOT_MAX_CHARS: usize = 80;

type CwdCallback = Box<dyn Fn(PaneId, Option<String>)>;
type FocusCallback = Rc<dyn Fn(PaneId)>;
type DropCallback = Rc<dyn Fn(PaneId, PaneId, crate::tree::DropSide)>;

/// Callbacks the host uses to observe a pane's terminal.
pub struct PaneSignals {
    pub menu: Option<crate::context_menu::Callbacks>,
    pub on_focus: Option<Box<dyn Fn(PaneId)>>,
    pub on_title: Option<Box<dyn Fn(PaneId, String)>>,
    pub on_exit: Option<Box<dyn Fn(PaneId, i32)>>,
    pub on_cwd: Option<CwdCallback>,
    /// A Ctrl+click asked for a link target to be opened.
    ///
    /// The pane owns the gesture but not the window the launch would be
    /// parented to, so the host performs the launch and can refuse a pane that
    /// is no longer live.
    pub on_open_link: Option<Rc<dyn Fn(PaneId, String)>>,
    /// A drag of this pane began.
    ///
    /// Shared rather than boxed because each rebuild attaches fresh handlers,
    /// and several of them refer to the same callback.
    pub on_drag_begin: Option<Rc<dyn Fn(PaneId)>>,
    /// A drag ended, wherever it ended.
    pub on_drag_end: Option<Rc<dyn Fn()>>,
    /// A pane was dropped on this pane, on the given side.
    pub on_drop: Option<DropCallback>,
}

/// Pane state shared with the signal handlers.
///
/// The handlers are GTK callbacks that carry no user data slot, so the state
/// they need to update is captured as a shared `Rc` rather than looked up from
/// the widget. That keeps the wiring sound and removes any need for a global
/// side table.
#[derive(Debug, Clone, Copy, Default)]
struct PaneMetrics {
    cell: Option<CellSize>,
    last_size: Option<TermSize>,
}

#[derive(Debug, Default)]
struct PaneState {
    metrics: PaneMetrics,
    cwd: Option<String>,
    focused: bool,
    /// The terminal's title, as reported by the program running in it.
    title: Option<String>,
    /// Set when the title changed while this pane was not focused.
    ///
    /// Programs use the title to advertise what they are doing — a build that
    /// finished, a shell waiting for input — so a change on an unfocused pane
    /// is worth noticing, and clearing it when the pane is focused keeps the
    /// marker honest.
    title_changed: bool,
}

/// One pane: a header area, a terminal, and a footer area.
///
/// The widget owns its terminal's PTY. Splitting or closing a pane destroys the
/// widget, and with it the process, which is why closing the last pane of a
/// workspace is escalated to closing the workspace instead.
pub struct Pane {
    id: PaneId,
    terminal: vte::Terminal,
    menu: Option<crate::context_menu::ContextMenu>,
    state: Rc<RefCell<PaneState>>,
    /// The container currently in the widget tree.
    ///
    /// Rebuilt on every layout change, so this is replaced rather than kept.
    /// Focus styling and slot rendering have to reach the live container, or
    /// they land on a widget nothing is showing.
    container: RefCell<Option<Container>>,
    /// The callback a newly built container's click gesture is wired to.
    ///
    /// The gesture itself is created per container: GTK lets a controller
    /// belong to exactly one widget, so reusing one across rebuilds aborts.
    focus_callback: RefCell<Option<FocusCallback>>,
    /// Drag handlers, re-attached to each new container.
    drag: RefCell<Option<DragHandlers>>,
}

/// The drag handlers a pane applies to each container it builds.
struct DragHandlers {
    on_drag_begin: Option<Rc<dyn Fn(PaneId)>>,
    on_drag_end: Option<Rc<dyn Fn()>>,
    on_drop: Option<DropCallback>,
}

/// The widgets wrapping one pane's terminal.
struct Container {
    /// Outer box, carrying the focused/exited state classes.
    frame: gtk::Box,
    /// The header strip, which is also the drag handle.
    header: gtk::Box,
    /// Where plugin content for this pane's header goes, beside the title.
    header_slot: gtk::Box,
    /// The terminal's own title, as reported through OSC escapes.
    title: gtk::Label,
    /// Slot area below the terminal.
    footer: gtk::Box,
}

fn button_at(widget: &gtk::Widget, x: f64, y: f64) -> bool {
    widget
        .pick(x, y, gtk::PickFlags::DEFAULT)
        .is_some_and(|target| {
            target.is::<gtk::Button>() || target.ancestor(gtk::Button::static_type()).is_some()
        })
}

impl Pane {
    /// Build a pane with a live shell in it.
    pub fn new(
        id: PaneId,
        mut signals: PaneSignals,
        cwd: Option<String>,
        on_spawn: crate::SpawnCallback,
    ) -> Result<Self, String> {
        let pty = crate::new_pty().map_err(|error| error.to_string())?;
        let terminal = vte::Terminal::builder()
            .pty(&pty)
            .scrollback_lines(10_000)
            .hexpand(true)
            .vexpand(true)
            .enable_bidi(true)
            .enable_shaping(true)
            .build();
        // Links belong to the terminal, not to the optional context menu: a
        // pane without a menu still offers Ctrl+click and link detection.
        crate::links::install(&terminal);
        // Started after the terminal exists, so the terminal can be told to
        // watch the child and therefore report its exit.
        let state = Rc::new(RefCell::new(PaneState {
            cwd: cwd.clone(),
            ..PaneState::default()
        }));
        let spawned_state = state.clone();
        crate::spawn_shell(
            &pty,
            &terminal,
            cwd,
            true,
            Rc::new(move |result| {
                if let Ok(cwd) = &result {
                    spawned_state.borrow_mut().cwd = cwd.clone();
                }
                on_spawn(result);
            }),
        );
        let menu = signals
            .menu
            .take()
            .map(|callbacks| crate::context_menu::ContextMenu::new(id, &terminal, callbacks));
        let pane = Self {
            id,
            terminal,
            menu,
            state,
            container: RefCell::new(None),
            focus_callback: RefCell::new(None),
            drag: RefCell::new(None),
        };
        pane.wire(signals);
        Ok(pane)
    }

    /// Build the container for this pane and remember it.
    ///
    /// Called on every rebuild of the layout. The terminal is reparented into
    /// the new container, which the pane then addresses for focus styling and
    /// slot content — a pane that kept its first container would style and
    /// measure a widget that is no longer on screen.
    /// Build the container for this pane from the signals it was created with.
    ///
    /// Kept as a field so a rebuild can re-attach the same handlers to the new
    /// container without the caller having to supply them again.
    /// `colour` is the palette index of the workspace the pane belongs to; the
    /// header marks itself with it so the colour is recognisable between the
    /// sidebar and the terminal area.
    pub fn build_container(
        &self,
        store: &SlotStore,
        colour: usize,
        restore: Option<Rc<dyn Fn()>>,
    ) -> gtk::Box {
        let self_id = self.id;
        // GTK rejects a widget that already has a parent, so the terminal is
        // detached before being placed in its new container.
        self.terminal.unparent();

        let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        frame.add_css_class("pane-frame");
        frame.add_css_class(&format!(
            "pane-workspace-colour-{}",
            colour % crate::workspace::PALETTE_SIZE
        ));

        // The header is always present: it carries the terminal's title, which
        // is how programs announce what they are doing, and it is the handle a
        // pane is dragged by.
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.add_css_class("pane-header");
        // The workspace's colour, as a marker on the header. It is a stripe
        // rather than a tint because terminal programs use colour to mean
        // something, and a colour behind their output would both reduce
        // legibility and read as application state.
        let marker = gtk::Box::new(gtk::Orientation::Vertical, 0);
        marker.add_css_class("workspace-stripe");
        marker.add_css_class(&format!(
            "workspace-colour-{}",
            colour % crate::workspace::PALETTE_SIZE
        ));
        header.append(&marker);
        let identity = gtk::Label::new(Some(&format!("#{}", self.id.0)));
        identity.add_css_class("pane-number");
        header.append(&identity);
        let title = gtk::Label::new(None);
        title.add_css_class("pane-title");
        title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        title.set_hexpand(true);
        title.set_halign(gtk::Align::Start);
        let header_slot = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header_slot.add_css_class("pane-slot");
        header.append(&title);
        header.append(&header_slot);
        if let Some(callback) = restore {
            let theme = gtk::IconTheme::for_display(&header.display());
            let button = if theme.has_icon("view-restore-symbolic") {
                gtk::Button::from_icon_name("view-restore-symbolic")
            } else {
                gtk::Button::with_label("Restore")
            };
            button.add_css_class("flat");
            button.add_css_class("pane-restore");
            button.set_tooltip_text(Some("Restore pane"));
            button.update_property(&[gtk::accessible::Property::Label("Restore pane")]);
            button.set_valign(gtk::Align::Center);
            button.connect_clicked(move |_| callback());
            header.append(&button);
        }

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        footer.add_css_class("pane-slot");
        footer.set_visible(false);
        footer.set_vexpand(false);

        frame.append(&header);
        frame.append(&self.terminal);
        frame.append(&footer);

        // A fresh gesture per container: a controller belongs to one widget, so
        // moving one between containers is not allowed.
        if let Some(callback) = self.focus_callback.borrow().as_ref().map(Rc::clone) {
            let controller = gtk::GestureClick::new();
            controller.set_propagation_phase(gtk::PropagationPhase::Capture);
            controller.connect_pressed(move |controller, _, x, y| {
                // header controls need their own keyboard focus, not a capture-
                // phase grab that redirects their activation into the terminal.
                if controller
                    .widget()
                    .is_some_and(|widget| button_at(&widget, x, y))
                {
                    return;
                }
                callback(self_id);
            });
            frame.add_controller(controller);
        }
        self.attach_drag(&frame, &header);

        *self.container.borrow_mut() = Some(Container {
            frame: frame.clone(),
            header,
            header_slot,
            title,
            footer,
        });
        self.apply_state();
        self.refresh_title();
        self.refresh_slots(store);
        // A rebuild puts the terminal in a new container, and GTK drops the
        // keyboard focus with the old one. Restoring it here is what keeps
        // typing going to the terminal the user was already working in.
        if self.state.borrow().focused {
            self.terminal.grab_focus();
        }
        frame
    }

    /// Push the focused and exited classes onto the live container.
    fn apply_state(&self) {
        let state = self.state.borrow();
        let container = self.container.borrow();
        let Some(container) = container.as_ref() else {
            return;
        };
        if state.focused {
            container.frame.add_css_class("focused");
        } else {
            container.frame.remove_css_class("focused");
        }
        // The marker for a changed title belongs only on an unfocused pane, so
        // looking at it is enough to clear it.
        if state.title_changed {
            container.header.add_css_class("title-changed");
        } else {
            container.header.remove_css_class("title-changed");
        }
    }

    fn wire(&self, signals: PaneSignals) {
        let id = self.id;
        let state = self.state.clone();
        let terminal = self.terminal.clone();
        let root_weak = glib::WeakRef::<gtk::Box>::new();

        self.terminal
            .connect_char_size_changed(move |term, width, height| {
                if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32
                {
                    return;
                }
                {
                    let mut borrowed = state.borrow_mut();
                    borrowed.metrics.cell = Some(CellSize {
                        width: width as i32,
                        height: height as i32,
                    });
                }
                reconcile(&state, &terminal, root_weak.upgrade().as_ref());
                let _ = term;
            });

        let state = self.state.clone();
        self.terminal
            .connect_current_directory_uri_changed(move |term| {
                let dir = term
                    .current_directory_uri()
                    .as_deref()
                    .and_then(file_uri_to_path);
                state.borrow_mut().cwd = dir.clone();
                if let Some(callback) = &signals.on_cwd {
                    callback(id, dir);
                }
            });

        let PaneSignals {
            menu: _,
            on_focus,
            on_open_link,
            on_title,
            on_exit,
            on_cwd: _,
            on_drag_begin,
            on_drag_end,
            on_drop,
        } = signals;
        *self.drag.borrow_mut() = Some(DragHandlers {
            on_drag_begin,
            on_drag_end,
            on_drop,
        });
        if let Some(callback) = on_title {
            self.terminal.connect_window_title_changed(move |term| {
                let title = term
                    .window_title()
                    .map(|t| t.to_string())
                    .unwrap_or_default();
                callback(id, title);
            });
        }
        if let Some(callback) = on_exit {
            self.terminal
                .connect_child_exited(move |_, status| callback(id, status));
        }
        if let Some(callback) = on_focus {
            let callback: Rc<dyn Fn(PaneId)> = Rc::from(callback);
            self.focus_callback.borrow_mut().replace(callback.clone());
            let focus = gtk::EventControllerFocus::new();
            let terminal = self.terminal.downgrade();
            focus.connect_enter(move |_| {
                // grab_focus during a rebuild can emit this while the host is
                // borrowed. Synchronize after the signal, never inside it.
                let callback = callback.clone();
                let terminal = terminal.clone();
                glib::idle_add_local_once(move || {
                    if terminal
                        .upgrade()
                        .is_some_and(|terminal| terminal.has_focus())
                    {
                        callback(id);
                    }
                });
            });
            self.terminal.add_controller(focus);
        }

        if let Some(callback) = on_open_link {
            self.attach_link_gesture(callback);
        }

        let state = self.state.clone();
        self.terminal.connect_map(move |terminal| {
            // Reconciliation is driven by the terminal being mapped, which
            // happens after the container around it has an allocation.
            reconcile(&state, terminal, None);
            if state.borrow().focused {
                terminal.grab_focus();
            }
        });

        self.attach_scroll();
    }

    /// Open a link on Ctrl+primary-click.
    ///
    /// Installed once, on the terminal itself: the terminal is created once and
    /// survives every container rebuild, so one controller serves every layout.
    /// It runs in the capture phase because the library's own click gesture
    /// runs in the bubble phase and claims a primary press for selection or for
    /// the child; a bubble-phase link handler would never see the press that is
    /// supposed to open a link.
    ///
    /// The sequence is claimed only when it will actually open something. Every
    /// other press is denied so the library handles it exactly as before: plain
    /// clicks, drags, other modifiers and clicks on text that is not an
    /// openable link never become the gesture's.
    fn attach_link_gesture(&self, callback: Rc<dyn Fn(PaneId, String)>) {
        let id = self.id;
        let terminal = self.terminal.downgrade();
        let gesture = gtk::GestureClick::new();
        gesture.set_button(gdk::BUTTON_PRIMARY);
        gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
        gesture.connect_pressed(move |gesture, presses, x, y| {
            let Some(terminal) = terminal.upgrade() else {
                return;
            };
            let ctrl = gesture
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK);
            let link = crate::links::at(&terminal, x, y);
            if !crate::links::activates(ctrl, gesture.current_button(), presses, link.as_ref()) {
                gesture.set_state(gtk::EventSequenceState::Denied);
                return;
            }
            // Claiming the press is what keeps it from reaching the terminal
            // program and from clearing an existing selection.
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let Some(link) = link {
                callback(id, link.uri);
            }
        });
        self.terminal.add_controller(gesture);
    }

    /// Take over touch surface scrolling on this terminal.
    ///
    /// The library's own controller is on the same widget in the bubble phase,
    /// so a capture-phase controller runs first and consuming an event stops it
    /// from reaching the library. Wheel clicks and horizontal scrolling are not
    /// consumed at all: there the library's own amount is already the one a
    /// wheel is expected to move.
    fn attach_scroll(&self) {
        let state = self.state.clone();
        let terminal = self.terminal.clone();
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
        scroll.connect_scroll(move |controller, _dx, dy| {
            if controller.unit() != gtk::gdk::ScrollUnit::Surface {
                return glib::Propagation::Proceed;
            }
            let cell_height = state
                .borrow()
                .metrics
                .cell
                .map(|cell| cell.height)
                .unwrap_or_default();
            if scroll_terminal(&terminal, cell_height, dy) {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        self.terminal.add_controller(scroll);
    }

    pub fn terminal(&self) -> &vte::Terminal {
        &self.terminal
    }

    pub fn invalidate_menu(&self) {
        if let Some(menu) = &self.menu {
            menu.invalidate();
        }
    }

    pub fn menu_open(&self) -> bool {
        self.menu.as_ref().is_some_and(|menu| menu.is_open())
    }

    pub fn menu_inspection(&self) -> Vec<String> {
        self.menu
            .as_ref()
            .map(|menu| menu.inspection())
            .unwrap_or_default()
    }

    /// Attach the drag source to the header and the drop target to the frame.
    ///
    /// The header is the handle rather than the whole pane: a drag started on
    /// the terminal body must stay the terminal's own text selection.
    fn attach_drag(&self, frame: &gtk::Box, header: &gtk::Box) {
        let handlers = self.drag.borrow_mut().take();
        let Some(handlers) = handlers else {
            return;
        };

        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::MOVE);
        let id = self.id;
        source.connect_prepare(move |source, x, y| {
            if source
                .widget()
                .is_some_and(|widget| button_at(&widget, x, y))
            {
                return None;
            }
            // The dragged pane's identity travels with the drag, so the target
            // does not need the application to remember what is in flight.
            let value = (id.0).to_value();
            Some(gdk::ContentProvider::for_value(&value))
        });
        if let Some(callback) = handlers.on_drag_begin.as_ref() {
            let callback = callback.clone();
            source.connect_drag_begin(move |_, _| callback(id));
        }
        if let Some(callback) = handlers.on_drag_end.as_ref() {
            let callback = callback.clone();
            source.connect_drag_end(move |_, _, _| callback());
        }
        header.add_controller(source);

        let target = gtk::DropTarget::new(gtk::glib::Type::U64, gdk::DragAction::MOVE);
        // The pending drop is shown on the pane itself, and the label is
        // removed as soon as the pointer leaves.
        let side = Rc::new(RefCell::new(None));
        let frame_for_enter = frame.clone();
        let side_for_enter = side.clone();
        target.connect_enter(move |_, x, y| {
            let next = crate::tree::drop_side(
                x as i32,
                y as i32,
                frame_for_enter.width(),
                frame_for_enter.height(),
            );
            *side_for_enter.borrow_mut() = Some(next);
            apply_drop_side(&frame_for_enter, Some(next), id);
            gdk::DragAction::MOVE
        });
        let frame_for_motion = frame.clone();
        let side_for_motion = side.clone();
        target.connect_motion(move |_, x, y| {
            let next = crate::tree::drop_side(
                x as i32,
                y as i32,
                frame_for_motion.width(),
                frame_for_motion.height(),
            );
            if *side_for_motion.borrow() != Some(next) {
                *side_for_motion.borrow_mut() = Some(next);
                apply_drop_side(&frame_for_motion, Some(next), id);
            }
            gdk::DragAction::MOVE
        });
        let frame_for_leave = frame.clone();
        let side_for_leave = side.clone();
        target.connect_leave(move |_| {
            *side_for_leave.borrow_mut() = None;
            apply_drop_side(&frame_for_leave, None, id);
        });

        if let Some(callback) = handlers.on_drop.as_ref().map(Rc::clone) {
            let frame_for_drop = frame.clone();
            let side_for_drop = side.clone();
            target.connect_drop(move |_, value, x, y| {
                apply_drop_side(&frame_for_drop, None, id);
                let Ok(source_id) = value.get::<u64>() else {
                    return false;
                };
                // The side is taken from the last motion when available, and
                // recomputed from the drop position otherwise.
                let side = side_for_drop.borrow().unwrap_or_else(|| {
                    crate::tree::drop_side(
                        x as i32,
                        y as i32,
                        frame_for_drop.width(),
                        frame_for_drop.height(),
                    )
                });
                callback(crate::tree::PaneId(source_id), id, side);
                true
            });
        }
        frame.add_controller(target);
        // Put the handlers back for the next rebuild.
        *self.drag.borrow_mut() = Some(handlers);
    }

    /// The working-directory URI exactly as the terminal reported it, so a
    /// wrong path can be traced to the URI or to the conversion of it.
    pub fn cwd_uri(&self) -> Option<String> {
        self.terminal.current_directory_uri().map(|u| u.to_string())
    }

    /// The workspace colour marked on this pane's header, for verification.
    ///
    /// Read back off the widget rather than remembered, so this reports what is
    /// actually shown.
    pub fn header_colour(&self) -> Option<usize> {
        let container = self.container.borrow();
        let container = container.as_ref()?;
        colour_of_tree(container.header.upcast_ref())
    }

    /// The size of the colour marker on this pane's header.
    ///
    /// Reported as text because a marker with no area is not drawn at all, which
    /// is indistinguishable from no marker without measuring it.
    pub fn header_stripe_size(&self) -> String {
        let container = self.container.borrow();
        let Some(container) = container.as_ref() else {
            return "no-container".into();
        };
        match colour_of_tree(container.header.upcast_ref()) {
            Some(_) => match container
                .header
                .first_child()
                .and_then(|first| first.compute_bounds(&container.header))
            {
                Some(rect) => format!("{}x{}", rect.width() as i32, rect.height() as i32),
                None => "no-bounds".into(),
            },
            None => "no-marker".into(),
        }
    }

    /// The drop indication currently shown on this pane, for verification.
    pub fn drop_indication(&self) -> Option<&'static str> {
        let container = self.container.borrow();
        let container = container.as_ref()?;
        for (class, name) in [
            ("drop-left", "left"),
            ("drop-right", "right"),
            ("drop-top", "top"),
            ("drop-bottom", "bottom"),
        ] {
            if container.frame.has_css_class(class) {
                return Some(name);
            }
        }
        None
    }

    /// Remove any drop indication from this pane.
    pub fn clear_drop_indication(&self) {
        if let Some(container) = self.container.borrow().as_ref() {
            apply_drop_side(&container.frame, None, self.id);
        }
    }

    /// The container currently in the widget tree, if one has been built.
    pub fn widget(&self) -> Option<gtk::Box> {
        self.container.borrow().as_ref().map(|c| c.frame.clone())
    }

    /// The size of this pane's terminal area, in pixels.
    ///
    /// Taken from the terminal rather than the container: the terminal is
    /// always parented and allocated, which is what decides the split
    /// orientation.
    pub fn extent(&self) -> (i32, i32) {
        let (w, h) = (self.terminal.width(), self.terminal.height());
        if w > 0 && h > 0 {
            return (w, h);
        }
        self.container
            .borrow()
            .as_ref()
            .map(|c| (c.frame.width(), c.frame.height()))
            .unwrap_or((0, 0))
    }

    /// Record the title the terminal has reported and show it.
    ///
    /// Programs set the title through OSC escapes to advertise state — a build
    /// in progress, a file open in an editor, a host being connected to. When
    /// that happens on a pane the user is not looking at, the header is marked
    /// so the change is noticed rather than silently rendered.
    pub fn set_title(&self, title: String) {
        let title = title.trim().to_string();
        {
            let mut state = self.state.borrow_mut();
            if state.title.as_deref() == Some(title.as_str()) {
                return;
            }
            state.title = Some(title);
            if !state.focused {
                state.title_changed = true;
            }
        }
        self.refresh_title();
        self.apply_state();
    }

    /// The terminal's current title, if it has reported one.
    pub fn title(&self) -> Option<String> {
        self.state.borrow().title.clone()
    }

    /// Whether the title changed while this pane was unfocused.
    pub fn title_changed(&self) -> bool {
        self.state.borrow().title_changed
    }

    fn refresh_title(&self) {
        let state = self.state.borrow();
        let container = self.container.borrow();
        let Some(container) = container.as_ref() else {
            return;
        };
        let text = title_text(state.title.as_deref(), self.id);
        container.title.set_text(&text);
    }

    /// Working directory last reported by the shell, or `None` while unknown.
    pub fn cwd(&self) -> Option<String> {
        self.state.borrow().cwd.clone()
    }

    /// The height of one text row in pixels, or zero before the terminal has
    /// been laid out.
    pub fn cell_height(&self) -> i32 {
        self.state
            .borrow()
            .metrics
            .cell
            .map(|cell| cell.height)
            .unwrap_or_default()
    }

    /// How far this terminal is scrolled back from its newest output, in lines.
    ///
    /// Zero while the terminal shows the most recent row, which is where a
    /// terminal starts and where typing returns it. The distance is measured
    /// against the library's newest row rather than the top of the history,
    /// because the newest row is the end a reader thinks of: a terminal scrolled
    /// back is showing older output.
    pub fn scroll_lines(&self) -> f64 {
        self.terminal
            .vadjustment()
            .map(|adjustment| (adjustment.upper() - adjustment.page_size()) - adjustment.value())
            .unwrap_or_default()
    }

    /// Mark this pane focused or not, and hand keyboard focus to its terminal.
    pub fn set_focused(&self, focused: bool) {
        {
            let mut state = self.state.borrow_mut();
            state.focused = focused;
            if focused {
                // Focusing the pane is the acknowledgement that the user has
                // seen whatever the title was announcing.
                state.title_changed = false;
            }
        }
        self.apply_state();
        // Focus is asserted every time rather than only on a change: a layout
        // rebuild reparents the terminal, which drops the keyboard focus, and a
        // pane that is already the focused one still needs it back.
        if focused {
            self.terminal.grab_focus();
        }
    }

    pub fn is_focused(&self) -> bool {
        self.state.borrow().focused
    }

    /// Refresh both slot areas from the store.
    pub fn refresh_slots(&self, store: &SlotStore) {
        let container = self.container.borrow();
        let Some(container) = container.as_ref() else {
            return;
        };
        slots::render_slot(
            store,
            Scope::PaneHeader(self.id),
            &container.header_slot,
            PANE_SLOT_MAX_CHARS,
        );
        slots::render_slot(
            store,
            Scope::PaneFooter(self.id),
            &container.footer,
            PANE_SLOT_MAX_CHARS,
        );
    }

    /// Re-derive the terminal's cell size from the current allocation.
    ///
    /// Called whenever the pane is mapped, which covers both a divider drag and
    /// a window resize. Deriving from the allocation each time rather than
    /// accumulating pixel deltas is what stops sizes drifting over a long
    /// session.
    pub fn reconcile(&self) -> Option<TermSize> {
        let frame = self.widget();
        reconcile(&self.state, &self.terminal, frame.as_ref())
    }

    /// The size currently reported to the terminal.
    pub fn reported_size(&self) -> Option<TermSize> {
        self.state.borrow().metrics.last_size
    }
}

/// Convert an allocation into a cell count and push it to the terminal.
///
/// A sub-cell change is deliberately not reported: pushing it would make
/// full-screen programs redraw while a divider is dragged within one cell.
fn reconcile(
    state: &Rc<RefCell<PaneState>>,
    terminal: &vte::Terminal,
    root: Option<&gtk::Box>,
) -> Option<TermSize> {
    let root = root?;
    // The terminal's own extent is what the text is laid out in; the pane may
    // include slot areas the terminal does not.
    let tw = terminal.width();
    let th = terminal.height();
    let (width, height) = if tw > 0 && th > 0 {
        (tw, th)
    } else {
        (root.width(), root.height())
    };
    if width <= 0 || height <= 0 {
        return None;
    }

    let cell = state.borrow().metrics.cell;
    let next = size::terminal_size(width, height, cell.unwrap_or_default());
    if size::size_changed(state.borrow().metrics.last_size, next) {
        terminal.set_size(next.columns, next.rows);
        state.borrow_mut().metrics.last_size = Some(next);
    }
    Some(next)
}

/// How many lines a touch surface delta scrolls the history by.
///
/// GTK scales a scrollable's own touch surface deltas by `MAGIC_SCROLL_FACTOR`
/// before converting them to pixels, and it converts pixels to lines through the
/// cell height; the two cancel into a distance per delta unit that does not
/// depend on how tall the pane is. The terminal library has no equivalent: it
/// multiplies every delta, wheel or surface, by a tenth of the pane's rows, so a
/// touchpad gesture travels several times further in a tall pane than a finger
/// moved that far in any other application.
///
/// A missing cell height yields no movement rather than a division by zero: the
/// caller hands the event back to the library, which is what this did before.
pub fn surface_scroll_lines(delta: f64, cell_height: i32) -> f64 {
    const SURFACE_FACTOR: f64 = 2.5;
    if cell_height <= 0 {
        return 0.0;
    }
    delta * SURFACE_FACTOR / f64::from(cell_height)
}

/// Whether the terminal's adjustment has history to move through.
///
/// The bounds are the library's: a value below the first history row and a
/// value one page above the last insert row are the limits, so anything at or
/// between them means the screen cannot move. A terminal drawing a full screen
/// reports its first row as both bounds, because that screen is not allowed to
/// scroll at all — which is exactly the case where the library turns the
/// gesture into arrow keys or button events instead.
pub fn has_scroll_room(lower: f64, upper: f64, page_size: f64) -> bool {
    upper - lower > page_size
}

/// Apply one touch surface scroll event to a terminal's history.
///
/// Returns whether the event was consumed. Everything except a vertical touch
/// surface delta on a terminal with history to move through is left to the
/// library, so a wheel click keeps its own amount and a program drawing a full
/// screen keeps receiving the gesture as arrow keys or button events.
pub fn scroll_terminal(terminal: &vte::Terminal, cell_height: i32, delta: f64) -> bool {
    // The adjustment exists only once the terminal is realized; before that there
    // is no history to move through either.
    let Some(adjustment) = terminal.vadjustment() else {
        return false;
    };
    let (oldest, page) = (adjustment.lower(), adjustment.page_size());
    let newest = adjustment.upper() - page;
    if !has_scroll_room(oldest, adjustment.upper(), page) {
        return false;
    }
    let lines = surface_scroll_lines(delta, cell_height);
    if lines == 0.0 {
        return false;
    }
    // The library refuses to move past the first row or past the newest one, so
    // the target is clamped to its own limits before it is set. A gesture that
    // lands where the terminal already is moves nothing, and the library is
    // better placed to report that than a handler claiming a move it did not
    // make.
    let target = (adjustment.value() + lines).clamp(oldest, newest.max(oldest));
    if (target - adjustment.value()).abs() < f64::EPSILON {
        return false;
    }
    adjustment.set_value(target);
    true
}

/// The palette index marked by a class on this widget or any descendant.
fn colour_of_tree(widget: &gtk::Widget) -> Option<usize> {
    for index in 0..crate::workspace::PALETTE_SIZE {
        if widget.has_css_class(&format!("workspace-colour-{index}")) {
            return Some(index);
        }
    }
    let mut child = widget.first_child();
    while let Some(node) = child {
        if let Some(found) = colour_of_tree(&node) {
            return Some(found);
        }
        child = node.next_sibling();
    }
    None
}

/// The label a pane header shows for a given terminal title.
///
/// A pane whose terminal has not named itself still needs a label, or the
/// header would look broken and two unnamed panes would be indistinguishable;
/// the pane's id is a stable, honest fallback.
fn title_text(title: Option<&str>, id: PaneId) -> String {
    match title {
        Some(title) if !title.trim().is_empty() => title.trim().to_string(),
        _ => format!("Terminal {}", id.0),
    }
}

/// Show or clear the pending drop on a pane.
///
/// The side is expressed as a CSS class so the stylesheet decides what the
/// indication looks like, and no widget is created or destroyed during a drag.
fn apply_drop_side(frame: &gtk::Box, side: Option<crate::tree::DropSide>, pane: PaneId) {
    use crate::tree::DropSide;
    for class in ["drop-left", "drop-right", "drop-top", "drop-bottom"] {
        frame.remove_css_class(class);
    }
    let class = match side {
        Some(DropSide::Left) => "drop-left",
        Some(DropSide::Right) => "drop-right",
        Some(DropSide::Top) => "drop-top",
        Some(DropSide::Bottom) => "drop-bottom",
        None => return,
    };
    frame.add_css_class(class);
    // Reported as it happens: a drag lasts a fraction of a second, which the
    // periodic state report is far too coarse to capture.
    if std::env::var_os("AMONG_SELFCHECK").is_some() {
        let name = match side {
            Some(DropSide::Left) => "left",
            Some(DropSide::Right) => "right",
            Some(DropSide::Top) => "top",
            Some(DropSide::Bottom) => "bottom",
            None => "none",
        };
        eprintln!("among[drop]: pane={} side={name}", pane.0);
    }
}

/// Convert a `file://` URI into a filesystem path.
///
/// GTK writes the local hostname into the authority component even for a local
/// path, so `file:///tmp/x` arrives as `file://myhost/tmp/x`. Everything before
/// the first slash is therefore dropped: a terminal's working directory is
/// always local, and keeping the authority would turn an absolute path into a
/// relative one, silently.
pub fn file_uri_to_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let path = match rest.find('/') {
        // No slash at all means there is no path component to speak of.
        None => return None,
        // A leading slash means the authority was empty: nothing to drop.
        Some(0) => rest,
        Some(slash) => &rest[slash..],
    };
    Some(percent_decode(path))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(value) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_surface_delta_scrolls_the_distance_it_reports() {
        // A 19 pixel row: one unit of surface delta moves 2.5 lines, which is the
        // 47.5 pixels GTK would have scrolled for the same gesture.
        let moved = surface_scroll_lines(19.0, 19);
        assert!((moved - 2.5).abs() < 1e-9);
        assert!((moved * 19.0 - 47.5).abs() < 1e-9);
        // The distance a gesture covers is the reported one whatever the row
        // height is, so the feel does not depend on the pane's size.
        assert!((surface_scroll_lines(40.0, 20) * 20.0 - 100.0).abs() < 1e-9);
    }

    #[test]
    fn small_deltas_stay_fractional_so_scrolling_accumulates() {
        let moved = surface_scroll_lines(0.5, 20);
        assert!(
            moved > 0.0 && moved < 1.0,
            "{moved} should be a fraction of a line"
        );
    }

    #[test]
    fn an_unknown_cell_height_moves_nothing_and_never_divides_by_zero() {
        assert_eq!(surface_scroll_lines(100.0, 0), 0.0);
        assert_eq!(surface_scroll_lines(100.0, -1), 0.0);
        assert!(surface_scroll_lines(f64::NAN, 19).is_nan());
    }

    #[test]
    fn a_terminal_with_history_has_room_and_one_without_it_does_not() {
        // 200 rows of history below a 40 row screen.
        assert!(has_scroll_room(0.0, 240.0, 40.0));
        // A full screen: its first row is both bounds, so there is nothing to move.
        assert!(!has_scroll_room(0.0, 40.0, 40.0));
        // A full screen that has scrolled inside itself still cannot move, because
        // its history is its own screen.
        assert!(!has_scroll_room(12.0, 52.0, 40.0));
        // Both ends of a scrolled-back history have room in the other direction,
        // so the decision rests on the bounds alone rather than on the position.
        assert!(has_scroll_room(0.0, 240.0, 40.0));
    }

    #[test]
    fn an_unnamed_pane_gets_a_placeholder() {
        assert_eq!(title_text(None, PaneId(3)), "Terminal 3");
        assert_eq!(title_text(Some(""), PaneId(1)), "Terminal 1");
        assert_eq!(title_text(Some("   "), PaneId(2)), "Terminal 2");
    }

    #[test]
    fn a_named_pane_shows_its_title_trimmed() {
        assert_eq!(title_text(Some("vim main.rs"), PaneId(1)), "vim main.rs");
        assert_eq!(title_text(Some("  ssh host  "), PaneId(2)), "ssh host");
    }

    #[test]
    fn a_hostname_in_the_uri_is_dropped_not_glued_to_the_path() {
        // GTK writes the local hostname into the authority component, so this
        // is the form a local directory actually arrives in. Keeping the host
        // would make the path relative and silently wrong.
        assert_eq!(
            file_uri_to_path("file://myhost/tmp/project"),
            Some("/tmp/project".to_string())
        );
        assert_eq!(
            file_uri_to_path("file:///tmp/project"),
            Some("/tmp/project".to_string())
        );
        assert_eq!(
            file_uri_to_path("file://localhost/tmp/project"),
            Some("/tmp/project".to_string())
        );
    }

    #[test]
    fn a_uri_without_a_path_is_rejected() {
        assert_eq!(file_uri_to_path("file://"), None);
        assert_eq!(file_uri_to_path("file://myhost"), None);
        assert_eq!(file_uri_to_path("https://example.com/x"), None);
    }

    #[test]
    fn an_absolute_path_is_not_made_relative_by_a_host_prefix() {
        let path = file_uri_to_path("file://bastion/srv/projects/repo")
            .expect("a local directory must resolve");
        assert!(path.starts_with('/'), "path came out relative: {path}");
    }

    #[test]
    fn file_uris_become_paths() {
        assert_eq!(
            file_uri_to_path("file:///home/demo/projects"),
            Some("/home/demo/projects".to_string())
        );
    }

    #[test]
    fn a_localhost_authority_is_dropped() {
        assert_eq!(
            file_uri_to_path("file://localhost/tmp"),
            Some("/tmp".to_string())
        );
    }

    #[test]
    fn percent_escapes_are_decoded() {
        assert_eq!(
            file_uri_to_path("file:///tmp/my%20dir"),
            Some("/tmp/my dir".to_string())
        );
    }

    #[test]
    fn a_non_file_uri_is_not_a_path() {
        assert_eq!(file_uri_to_path("https://example.com"), None);
        assert_eq!(file_uri_to_path("/already/a/path"), None);
    }

    #[test]
    fn a_trailing_percent_is_left_alone() {
        // A malformed escape must not lose the rest of the path.
        assert_eq!(
            file_uri_to_path("file:///tmp/100%"),
            Some("/tmp/100%".into())
        );
    }

    #[test]
    fn percent_sequences_are_decoded_case_insensitively() {
        assert_eq!(percent_decode("%2F%2f"), "//");
    }
}
