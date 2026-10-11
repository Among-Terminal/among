use among_tools::sessions::rectangles;

#[test]
fn pane_geometry_cannot_be_replaced_by_a_sidebar_row_with_the_same_number() {
    let text="origin=320,79 tree_boxes=[1:0+0+600x400 2:601+0+300x400] sidebar_rows=[1:0+40+320x40] name_rects=[1:17+46+150x28]";
    assert_eq!(
        among_tools::sessions::pane_rectangles(text, "1"),
        vec![[0.0, 0.0, 600.0, 400.0]]
    );
    assert!(among_tools::sessions::pane_rectangles("name_rects=[1:17+46+150x28]", "1").is_empty());
}

#[test]
fn consent_rectangles_require_complete_values_and_sparse_ids_do_not_match_suffixes() {
    assert_eq!(
        rectangles("Restore:1.5+2+30x40 Restore:1.5+2+30x40", "Restore"),
        vec![[1.5, 2.0, 30.0, 40.0]; 2]
    );
    assert!(rectangles("Restore:1+2+30x", "Restore").is_empty());
    assert_eq!(
        rectangles("1000000000000:0+0+600x400 2:601+0+300x400", "2"),
        vec![[601.0, 0.0, 300.0, 400.0]]
    );
    assert!(rectangles("1000000000000:0+0+600x400", "0").is_empty());
}

#[test]
fn actual_sessions_cli_refuses_unowned_display_before_creating_fixtures() -> among_tools::Result<()>
{
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .env("HOME", "/synthetic-personal-home")
        .args(["check-sessions", "/bin/true"])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains("owned display runner"));
    Ok(())
}
