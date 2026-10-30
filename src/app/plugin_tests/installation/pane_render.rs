use super::*;

#[test]
#[ignore = "requires an isolated native display, xdotool and ImageMagick"]
fn long_unicode_pane_status_preserves_admitted_text_and_tooltip_in_a_narrow_window() {
    let app = application("com.github.among-terminal.long-pane-status-test");
    let ctx = build_with_writer(&app, None, crate::session::Writer::new).unwrap();
    wait_for(|| ctx.state.borrow().session_ready, Duration::from_secs(5));
    ctx.window.unmaximize();
    ctx.window.set_default_size(640, 480);
    wait_for(
        || !ctx.window.is_maximized() && ctx.window.width() <= 700,
        Duration::from_secs(3),
    );
    let path = format!("/данные/{}", "日本語🙂".repeat(145));
    let text = path.chars().take(480).collect::<String>();
    assert!(text.chars().count() > 80);
    assert!(text.chars().count() <= crate::plugin::protocol::MAX_TEXT_CHARS);
    let scope = Scope::PaneFooter(PaneId(1));
    let mut fragment = crate::slots::Fragment::text("renderer-regression", text.clone());
    fragment.tooltip = Some(path.clone());
    ctx.state.borrow_mut().store.set(
        scope,
        "renderer-regression",
        crate::slots::Contribution {
            source: "renderer-regression".into(),
            fragments: vec![fragment],
            refresh: crate::slots::Refresh::OnEvent,
            last_asked_ms: None,
        },
    );
    refresh_slots(&ctx);
    let pane = ctx
        .state
        .borrow()
        .pane(PaneId(1))
        .unwrap()
        .widget()
        .unwrap();
    let find = || {
        widgets(pane.upcast_ref())
            .into_iter()
            .filter_map(|widget| widget.downcast::<gtk::Label>().ok())
            .find(|label| label.text() == text && label.is_mapped() && label.width() > 0)
    };
    wait_for(
        || find().is_some_and(|label| label.layout().is_ellipsized()),
        Duration::from_secs(3),
    );
    let label = find().unwrap();
    assert_eq!(label.text(), text);
    assert_eq!(
        label.parent().unwrap().tooltip_text().as_deref(),
        Some(path.as_str())
    );
    assert!(label.height() > 0);
    for ancestor in [pane.upcast_ref::<gtk::Widget>(), ctx.window.upcast_ref()] {
        let bounds = label.compute_bounds(ancestor).unwrap();
        let viewport =
            gtk::graphene::Rect::new(0.0, 0.0, ancestor.width() as f32, ancestor.height() as f32);
        assert!(
            viewport.contains_rect(&bounds),
            "label must remain inside the narrow pane/window: {bounds:?}"
        );
    }
    assert!(ctx.window.width() <= 700);
    assert!(
        ctx.state
            .borrow()
            .pane(PaneId(1))
            .unwrap()
            .terminal()
            .width()
            > 0
    );
    screenshot(&ctx.window, "long-unicode-pane-status-narrow");
    ctx.window.destroy();
    println!("PASS renderer-only synthetic contribution: exact 480-character Unicode label, full tooltip, mapped/allocated and visually ellipsized inside narrow pane/window; not installed plugin acceptance");
}
