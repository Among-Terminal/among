use std::fs;
use std::process::Command;

#[test]
fn actual_cli_freezes_checks_and_refuses_source_drift() -> among_tools::Result<()> {
    let root = among_tools::storage::private_directory(&std::env::temp_dir(), "among-tools-test")?;
    fs::create_dir_all(root.join("scripts"))?;
    fs::write(
        root.join("scripts/check-example.py"),
        "import json\nassert actual == expected\n",
    )?;
    fs::write(root.join("README.md"), "python3 scripts/check-example.py\n")?;
    let snapshot = root.join("inventory.json");
    let binary = env!("CARGO_BIN_EXE_among-tools");
    let run = |operation: &str| {
        Command::new(binary)
            .arg("inventory")
            .arg(operation)
            .arg(&root)
            .arg(&snapshot)
            .output()
    };
    assert!(run("freeze")?.status.success());
    let inventory: among_tools::inventory::Inventory =
        serde_json::from_slice(&fs::read(&snapshot)?)?;
    assert_eq!(inventory.sources.len(), 1);
    assert_eq!(
        inventory.sources["scripts/check-example.py"].callers[0].path,
        "README.md"
    );
    assert_eq!(
        inventory.sources["scripts/check-example.py"]
            .scenarios_and_assertions
            .len(),
        1
    );
    assert!(run("check")?.status.success());
    assert!(!run("freeze")?.status.success());
    fs::write(
        root.join("README.md"),
        "new caller: python3 scripts/check-example.py\n",
    )?;
    assert!(!run("check")?.status.success());
    fs::write(root.join("README.md"), "python3 scripts/check-example.py\n")?;
    fs::write(
        root.join("scripts/new-helper.py"),
        "def imported_helper():\n    return 1\n",
    )?;
    assert!(!run("check")?.status.success());
    fs::remove_file(root.join("scripts/new-helper.py"))?;
    fs::write(root.join("scripts/check-example.py"), "assert weakened\n")?;
    assert!(!run("check")?.status.success());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn help_works_without_display_and_unknown_commands_fail() -> Result<(), Box<dyn std::error::Error>>
{
    let binary = env!("CARGO_BIN_EXE_among-tools");
    let help = Command::new(binary).env_clear().arg("--help").output()?;
    assert!(help.status.success());
    assert!(String::from_utf8(help.stdout)?.contains("inventory check"));
    assert!(!Command::new(binary)
        .env_clear()
        .arg("unported-operation")
        .output()?
        .status
        .success());
    Ok(())
}
