use std::process::Command;

#[test]
fn help_works_without_display_and_unknown_commands_fail() -> Result<(), Box<dyn std::error::Error>>
{
    let binary = env!("CARGO_BIN_EXE_among-tools");
    let help = Command::new(binary).env_clear().arg("--help").output()?;
    assert!(help.status.success());
    assert!(String::from_utf8(help.stdout)?.contains("platform discover"));
    assert!(!Command::new(binary)
        .env_clear()
        .arg("unported-operation")
        .output()?
        .status
        .success());
    Ok(())
}
