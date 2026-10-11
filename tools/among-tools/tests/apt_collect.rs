use among_tools::{apt_collect, Result};
use serde_json::json;

#[test]
fn source_enablement_retains_other_fields_and_is_idempotent_without_final_newline() -> Result<()> {
    for input in [
        "Types: deb\nURIs: https://archive.ubuntu.com/ubuntu\nSuites: noble\n",
        "Types: deb",
    ] {
        let output = apt_collect::enable_sources(input)?;
        assert!(output.contains("Types: deb deb-src"));
        assert_eq!(apt_collect::enable_sources(&output)?, output);
    }
    let text = "# Types: deb\nTypes: deb deb-src\nEnabled: no\n";
    assert_eq!(apt_collect::enable_sources(text)?, text);
    Ok(())
}
#[test]
fn real_apt_list_names_bind_exact_signed_release_index_member_and_role() -> Result<()> {
    assert_eq!(
        apt_collect::index_identity(
            "archive.ubuntu.com_ubuntu_dists_noble_main_binary-amd64_Packages.lz4"
        )?,
        Some(
            json!({"file":"archive.ubuntu.com_ubuntu_dists_noble_main_binary-amd64_Packages","release":"archive.ubuntu.com_ubuntu_dists_noble_InRelease","release_member":"main/binary-amd64/Packages","role":"binary"})
        )
    );
    let source = apt_collect::index_identity(
        "ports.ubuntu.com_ubuntu-ports_dists_noble-updates_universe_source_Sources",
    )?
    .ok_or("source index absent")?;
    assert_eq!(source["role"], "source");
    assert_eq!(source["release_member"], "universe/source/Sources");
    assert!(apt_collect::index_identity("lock")?.is_none());
    assert!(apt_collect::index_identity("prefix_dists_noble__Sources").is_err());
    assert!(apt_collect::index_identity("../prefix_dists_noble_main_source_Sources").is_err());
    Ok(())
}
#[test]
fn actual_collection_cli_refuses_unprivileged_execution_before_output_creation() -> Result<()> {
    let output =
        std::env::temp_dir().join(format!("among-apt-collect-refused-{}", std::process::id()));
    assert!(!output.exists());
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("collect-apt-provenance")
        .arg(&output)
        .output()?;
    assert!(!result.status.success());
    assert!(!output.exists());
    assert!(String::from_utf8(result.stderr)?.contains("disposable root audit container"));
    Ok(())
}
