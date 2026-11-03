use super::replay;
use crate::plugin_install::release::fixture::Fixture;
use crate::{storage, Result};
use serde_json::json;
use std::os::unix::ffi::OsStrExt;

// Every profile carries a valid schema2 release observation, so each refusal
// below comes from the replay check it names rather than from absent inputs.
fn profile(architecture: &str, installed: &[u8], settings: &[u8]) -> Result<Fixture> {
    let fixture = Fixture::new()?;
    std::fs::create_dir(fixture.root.join("managed"))?;
    std::fs::write(
        fixture.root.join("profile.json"),
        serde_json::to_vec(&json!({"kind":"observed_published_installation_profile",
            "source_revision":fixture.inputs["source_revision"],
            "catalogue_revision":fixture.inputs["catalogue_revision"],
            "architecture":architecture,"installed_size":installed.len(),
            "installed_sha256":storage::sha256(installed)?,"settings_size":settings.len(),
            "settings_sha256":storage::sha256(settings)?}))?,
    )?;
    Ok(fixture)
}

fn refusal(fixture: &Fixture) -> String {
    let destination = fixture.root.join("destination");
    let error = match replay(&fixture.root, &destination) {
        Ok(()) => String::from("replay accepted a profile it must refuse"),
        Err(error) => error.to_string(),
    };
    assert!(!destination.exists(), "{error}");
    error
}

#[test]
fn renderer_replay_refuses_wrong_actual_architecture_before_writing_a_profile() -> Result<()> {
    let architecture = crate::platform::discover()?.architecture;
    let other = if architecture == "amd64" {
        "arm64"
    } else {
        "amd64"
    };
    let fixture = profile(other, b"{}", b"{}")?;
    assert_eq!(
        refusal(&fixture),
        "runtime replay provenance or actual architecture differs"
    );
    Ok(())
}

#[test]
fn renderer_replay_consumes_both_declared_file_digests_without_selecting_corrupt_bytes(
) -> Result<()> {
    let architecture = crate::platform::discover()?.architecture;
    for path in ["managed/installed.json", "managed-plugins.json"] {
        let fixture = profile(&architecture, b"{}", b"{}")?;
        std::fs::write(fixture.root.join("managed/installed.json"), b"{}")?;
        std::fs::write(fixture.root.join("managed-plugins.json"), b"{}")?;
        std::fs::write(fixture.root.join(path), b"[]")?;
        assert_eq!(
            refusal(&fixture),
            "release input size/SHA256 differs from its pinned declaration",
            "{path}"
        );
    }
    Ok(())
}

#[test]
fn renderer_replay_refuses_a_fifo_without_blocking_or_creating_selection() -> Result<()> {
    let fixture = profile(&crate::platform::discover()?.architecture, b"{}", b"{}")?;
    let path = std::ffi::CString::new(
        fixture
            .root
            .join("managed/installed.json")
            .as_os_str()
            .as_bytes(),
    )?;
    assert_eq!(unsafe { nix::libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let start = std::time::Instant::now();
    assert_eq!(
        refusal(&fixture),
        "release input is non-regular or exceeds its byte limit"
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
    Ok(())
}
