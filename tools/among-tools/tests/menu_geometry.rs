use among_tools::menu_geometry::{row_samples, settled_row};
fn snapshot(pane: u64, y: i64, size: &str, enabled: bool) -> String {
    format!("among[tick70]: menu_items=[({pane}, [\"Copy:false:1561+757+203x24\", \"Expand Pane:{enabled}:1561+{y}+{size}\"])]\n")
}
#[test]
fn partial_new_invocation_never_reuses_old_bounds() -> among_tools::Result<()> {
    let old = snapshot(3, 906, "97x24", true).repeat(2);
    let partial="among[tick70]: terminal_menus=[3]\namong[tick70]: menu_items=[(3, [\"icon:16x24\", \"Copy:false:1561+757+203x24\", ";
    let log = format!("{old}{partial}");
    assert!(settled_row(&log[old.len()..], 3, "Expand Pane")?.is_none());
    let log = format!("{old}{}", snapshot(3, 893, "97x24", true).repeat(2));
    assert_eq!(
        settled_row(&log[old.len()..], 3, "Expand Pane")?,
        Some([1561, 893, 97, 24])
    );
    Ok(())
}
#[test]
fn geometry_requires_two_complete_equal_positive_samples() -> among_tools::Result<()> {
    let valid = snapshot(3, 893, "97x24", true);
    assert!(settled_row(&snapshot(3, 893, "0x0", true).repeat(2), 3, "Expand Pane")?.is_none());
    assert!(settled_row(
        &format!("{}{valid}", snapshot(3, 906, "97x24", true)),
        3,
        "Expand Pane"
    )?
    .is_none());
    assert_eq!(
        settled_row(
            &format!("{}{}", snapshot(3, 906, "97x24", true), valid.repeat(2)),
            3,
            "Expand Pane"
        )?,
        Some([1561, 893, 97, 24])
    );
    assert!(row_samples(valid.trim_end(), 3, "Expand Pane")?.is_empty());
    assert!(settled_row(&valid.repeat(2), 3, "Pane")?.is_none());
    assert!(settled_row(&valid, 3, "Expand Pane")?.is_none());
    Ok(())
}
#[test]
fn disabled_closed_wrong_target_and_malformed_samples_do_not_fall_back() -> among_tools::Result<()>
{
    let valid = snapshot(3, 893, "97x24", true).repeat(2);
    assert!(settled_row(&valid, 2, "Expand Pane")?.is_none());
    for tail in [
        snapshot(3, 893, "97x24", false),
        "among[tick71]: menu_items=[]\n".into(),
        snapshot(2, 893, "97x24", true),
    ] {
        assert!(settled_row(&format!("{valid}{tail}"), 3, "Expand Pane")?.is_none());
    }
    assert!(row_samples("among[tick]: menu_items=invalid\n", 3, "Expand Pane").is_err());
    assert_eq!(
        settled_row(&snapshot(3, -12, "97x24", true).repeat(2), 3, "Expand Pane")?,
        Some([1561, -12, 97, 24])
    );
    Ok(())
}
