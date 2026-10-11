use among_tools::preferences;
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn all_ten_original_phases_and_file_preservation_assertions_remain() -> among_tools::Result<()> {
    assert_eq!(
        preferences::PHASES,
        [
            "edit",
            "restore",
            "fault",
            "disable",
            "off",
            "failure",
            "restore",
            "invalid-shortcuts",
            "invalid-version",
            "invalid-plugins"
        ]
    );
    let settings = json!({"keep":42,"plugins":[{}, {"name":"unavailable","enabled":true}]});
    let before = BTreeMap::from([("plugins.json".into(), serde_json::to_vec(&settings)?)]);
    for phase in ["restore", "off", "fault"] {
        preferences::verify(phase, &before, &before)?;
        let mut after = before.clone();
        after.insert("shortcuts.json".into(), b"changed".to_vec());
        assert!(preferences::verify(phase, &before, &after).is_err());
    }
    let lost = BTreeMap::from([("plugins.json".into(), b"{}".to_vec())]);
    assert!(preferences::verify("edit", &before, &lost).is_err());
    preferences::verify("invalid-plugins", &before, &lost)?;
    Ok(())
}

#[test]
fn actual_cli_refuses_unowned_display_before_touching_configuration() -> among_tools::Result<()> {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .env("HOME", "/synthetic-personal-home")
        .args(["check-preferences", "/bin/true", "/no-plugins"])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains("owned display runner"));
    Ok(())
}
