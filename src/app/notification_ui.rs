use super::*;
use among::plugin_notifications::{ActionHandle, Backend, Contribution, Icon, Owner, Target};
use std::collections::BTreeMap;
use std::time::Instant;

struct Row {
    revision: u64,
    widget: gtk::Box,
}

#[derive(Default)]
pub(super) struct Renderer {
    rows: BTreeMap<(Owner, String), Row>,
}

fn detach(widget: &impl IsA<gtk::Widget>) {
    if let Some(parent) = widget
        .parent()
        .and_then(|parent| parent.downcast::<gtk::Box>().ok())
    {
        parent.remove(widget);
    }
}

fn area(container: &gtk::Box) -> gtk::Box {
    let mut child = container.first_child();
    while let Some(widget) = child {
        if widget.has_css_class("notification-contributions") {
            if let Ok(scroll) = widget.clone().downcast::<gtk::ScrolledWindow>() {
                if let Some(area) = scroll_content(&scroll) {
                    return area;
                }
            }
        }
        child = widget.next_sibling();
    }
    let area = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    // Keep a bounded plugin area instead of making the native title/terminal
    // follow the sum of all producer/action widths. GTK scrolls keyboard focus.
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_width(true)
        .propagate_natural_height(true)
        .max_content_width(600)
        .child(&area)
        .build();
    scroll.add_css_class("notification-contributions");
    container.append(&scroll);
    area
}

fn scroll_content(scroll: &gtk::ScrolledWindow) -> Option<gtk::Box> {
    let child = scroll.child()?;
    if let Some(viewport) = child.downcast_ref::<gtk::Viewport>() {
        viewport.child()?.downcast().ok()
    } else {
        child.downcast().ok()
    }
}

fn row(item: &Contribution, backend: &Rc<RefCell<Backend>>) -> gtk::Box {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    row.add_css_class("status-fragment");
    row.add_css_class("notification-contribution");
    row.update_property(&[gtk::accessible::Property::Label(&format!(
        "{}: {}",
        item.owner.identity, item.text
    ))]);
    let style = match item.style {
        among::plugin_notifications::Style::Default => "status",
        among::plugin_notifications::Style::Success => "success",
        among::plugin_notifications::Style::Warning => "warning",
        among::plugin_notifications::Style::Error => "error",
        among::plugin_notifications::Style::Accent => "accent",
        among::plugin_notifications::Style::Dim => "dim-label",
    };
    row.add_css_class(style);
    let image = match &item.icon {
        Some(Icon::Theme(name)) if slots::icon_exists(name) => {
            Some(gtk::Image::from_icon_name(name))
        }
        Some(Icon::Raster(bytes)) => gtk::gdk::Texture::from_bytes(&glib::Bytes::from(&bytes[..]))
            .ok()
            .map(|texture| gtk::Image::from_paintable(Some(&texture))),
        _ => None,
    };
    if let Some(image) = image {
        image.set_pixel_size(16);
        image.set_accessible_role(gtk::AccessibleRole::Presentation);
        row.append(&image);
    }
    let text = gtk::Label::new(Some(&item.text));
    text.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.set_max_width_chars(SLOT_MAX_CHARS as i32);
    row.append(&text);
    row.set_tooltip_text(item.tooltip.as_deref());
    for action in &item.actions {
        let button = gtk::Button::with_label(&action.label);
        if let Some(label) = button
            .child()
            .and_then(|child| child.downcast::<gtk::Label>().ok())
        {
            label.set_max_width_chars(24);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        }
        button.add_css_class("flat");
        button.set_widget_name("notification-contribution-action");
        button.update_property(&[gtk::accessible::Property::Label(&action.label)]);
        button.update_property(&[gtk::accessible::Property::Description(&format!(
            "Notification action from {}",
            item.owner.identity
        ))]);
        let handle: ActionHandle = action.handle.clone();
        let backend = Rc::downgrade(backend);
        button.connect_clicked(move |_| {
            if let Some(backend) = backend.upgrade() {
                if backend
                    .borrow_mut()
                    .activate(&handle, Instant::now())
                    .is_err()
                {
                    eprintln!("among: notification action is no longer current or available");
                }
            }
        });
        row.append(&button);
    }
    row
}

pub(super) fn refresh(ctx: &Window) {
    // GTK removal/focus signals can re-enter the host. Hold no AppState/core/
    // backend borrow while creating or moving widgets; handles remain opaque.
    let (backend, targets) = {
        let state = ctx.state.borrow();
        let Some(bells) = &state.bells else {
            return;
        };
        let mut targets = BTreeMap::new();
        targets.insert(("app_header".to_owned(), 0), ctx.app_header.clone());
        targets.insert(("app_footer".to_owned(), 0), ctx.app_footer.clone());
        targets.insert(
            (
                "workspace_header".to_owned(),
                state.workspaces.active().reference,
            ),
            ctx.workspace_header.clone(),
        );
        for (index, workspace) in state.workspaces.workspaces().iter().enumerate() {
            if !state.sidebar_collapsed {
                if let Some(content) = ctx
                    .workspace_list
                    .row_at_index(index as i32)
                    .and_then(|row| row.child())
                    .and_then(|child| child.downcast::<gtk::Box>().ok())
                {
                    targets.insert(
                        ("workspace_summary".to_owned(), workspace.reference),
                        content,
                    );
                }
            }
        }
        for (id, pane) in &state.panes {
            if let Some((header, footer)) = pane.notification_slots() {
                targets.insert(("pane_header".to_owned(), id.0), header);
                targets.insert(("pane_footer".to_owned(), id.0), footer);
            }
        }
        (bells.notification_backend(), targets)
    };
    let snapshots = backend.borrow().contributions(Instant::now());
    let mut renderer = ctx.notification_renderer.borrow_mut();
    renderer.rows.retain(|key, row| {
        let current = snapshots.iter().any(|item| {
            key == &(item.owner.clone(), item.id.clone()) && row.revision == item.revision
        });
        if !current {
            detach(&row.widget);
        }
        current
    });
    for item in snapshots {
        let reference = match item.scope.target {
            Target::Application => 0,
            Target::Workspace { reference } | Target::Pane { reference } => reference,
        };
        let key = (item.owner.clone(), item.id.clone());
        let Some(container) = targets.get(&(item.scope.slot.clone(), reference)) else {
            if let Some(row) = renderer.rows.get(&key) {
                detach(&row.widget);
            }
            continue;
        };
        let target = area(container);
        let view = renderer.rows.entry(key).or_insert_with(|| Row {
            revision: item.revision,
            widget: row(&item, &backend),
        });
        if view.widget.parent().as_ref() != Some(target.upcast_ref()) {
            detach(&view.widget);
            target.append(&view.widget);
        }
    }
    for ((slot, _), container) in targets {
        let mut child = container.first_child();
        let mut visible = false;
        while let Some(widget) = child {
            if widget.has_css_class("notification-contributions") {
                widget.set_visible(
                    widget
                        .downcast_ref::<gtk::ScrolledWindow>()
                        .and_then(scroll_content)
                        .is_some_and(|area| area.first_child().is_some()),
                );
            }
            visible |= widget.is_visible();
            child = widget.next_sibling();
        }
        if slot != "workspace_summary" {
            container.set_visible(visible);
        }
    }
}
