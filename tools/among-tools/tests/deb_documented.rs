use among_tools::{deb_lifecycle, Result};
use std::path::Path;

fn guide() -> Result<String> {
    Ok(std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/debian-packaging.md"),
    )?)
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

/// Installed acceptance replays these exact commands; if the guide stops
/// parsing, the release run fails before installing anything, so the parse of
/// the committed guide is checked on every pull request.
#[test]
fn committed_guide_yields_the_four_replayed_commands() -> Result<()> {
    let commands = deb_lifecycle::documented_commands(&guide()?)?;
    assert_eq!(
        commands.install,
        words("apt install ./among-terminal-debian13-amd64.deb")
    );
    assert_eq!(
        commands.downgrade,
        words("apt install --allow-downgrades ./among-terminal-debian13-amd64.deb")
    );
    assert_eq!(commands.remove, words("apt remove among-terminal"));
    assert_eq!(commands.purge, words("apt purge among-terminal"));
    assert_eq!(commands.file()?, "among-terminal-debian13-amd64.deb");
    Ok(())
}

/// Each edit would make the guide promise something the replay does not run,
/// or make the replay run something the guide does not say. The refusal must
/// name the reason.
#[test]
fn guide_edits_that_break_the_replay_are_refused() -> Result<()> {
    let guide = guide()?;
    let install = "sudo apt purge among-terminal                          # purge";
    assert!(guide.contains(install), "guide purge line changed");
    let cases = [
        (
            "unknown command",
            guide.replace(install, "sudo apt autoremove --purge among-terminal"),
            "unknown documented command",
        ),
        (
            "command without sudo",
            guide.replace(install, "apt purge among-terminal"),
            "not run with sudo",
        ),
        (
            "missing purge",
            guide.replace(install, ""),
            "lacks the purge command",
        ),
        (
            "two different install files",
            guide.replacen(
                "sudo apt install ./among-terminal-debian13-amd64.deb",
                "sudo apt install ./among-terminal-ubuntu24.04-arm64.deb",
                1,
            ),
            "documented command varies",
        ),
        (
            "renamed section",
            guide.replace("## Installing a built package", "## Installing a package"),
            "lacks its installation section",
        ),
    ];
    for (name, text, reason) in cases {
        let error = deb_lifecycle::documented_commands(&text)
            .err()
            .ok_or(format!("{name} was accepted"))?
            .to_string();
        assert!(error.contains(reason), "{name}: {error}");
    }
    Ok(())
}
