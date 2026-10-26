use among_tools::{live_status::profile, storage, Result};
use serde_json::json;
use std::os::unix::ffi::OsStrExt;

struct Fixture(std::path::PathBuf);

impl Fixture {
    fn new() -> Result<Self> {
        std::fs::create_dir_all(env!("CARGO_TARGET_TMPDIR"))?;
        let root = storage::private_directory(
            std::path::Path::new(env!("CARGO_TARGET_TMPDIR")),
            "renderer-profile",
        )?;
        std::fs::create_dir(root.join("managed"))?;
        Ok(Self(root))
    }

    fn metadata(&self, architecture: &str, installed: &[u8], settings: &[u8]) -> Result<()> {
        std::fs::write(
            self.0.join("profile.json"),
            serde_json::to_vec(
                &json!({"kind":"observed_published_installation_profile","source_revision":among_tools::plugin_install::release::SOURCE,"catalogue_revision":among_tools::plugin_install::release::CATALOGUE,"architecture":architecture,"installed_size":installed.len(),"installed_sha256":storage::sha256(installed)?,"settings_size":settings.len(),"settings_sha256":storage::sha256(settings)?}),
            )?,
        )?;
        Ok(())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn renderer_replay_refuses_wrong_actual_architecture_before_writing_a_profile() -> Result<()> {
    let fixture = Fixture::new()?;
    let architecture = among_tools::platform::discover()?.architecture;
    fixture.metadata(
        if architecture == "amd64" {
            "arm64"
        } else {
            "amd64"
        },
        b"{}",
        b"{}",
    )?;
    let destination = fixture.0.join("destination");
    assert!(profile::replay(&fixture.0, &destination).is_err());
    assert!(!destination.exists());
    Ok(())
}

#[test]
fn renderer_replay_consumes_both_declared_file_digests_without_selecting_corrupt_bytes(
) -> Result<()> {
    for path in ["managed/installed.json", "managed-plugins.json"] {
        let fixture = Fixture::new()?;
        let architecture = among_tools::platform::discover()?.architecture;
        fixture.metadata(&architecture, b"{}", b"{}")?;
        std::fs::write(fixture.0.join("managed/installed.json"), b"{}")?;
        std::fs::write(fixture.0.join("managed-plugins.json"), b"{}")?;
        std::fs::write(fixture.0.join(path), b"[]")?;
        let destination = fixture.0.join("destination");
        assert!(profile::replay(&fixture.0, &destination).is_err());
        assert!(!destination.exists());
    }
    Ok(())
}

#[test]
fn renderer_replay_refuses_a_fifo_without_blocking_or_creating_selection() -> Result<()> {
    let fixture = Fixture::new()?;
    fixture.metadata(
        &among_tools::platform::discover()?.architecture,
        b"{}",
        b"{}",
    )?;
    let path = std::ffi::CString::new(
        fixture
            .0
            .join("managed/installed.json")
            .as_os_str()
            .as_bytes(),
    )?;
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let destination = fixture.0.join("destination");
    let start = std::time::Instant::now();
    assert!(profile::replay(&fixture.0, &destination).is_err());
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
    assert!(!destination.exists());
    Ok(())
}
