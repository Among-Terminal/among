use among_tools::{apt_collect, storage, Result};
use serde_json::json;
use std::fs;

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

/// Run a program the collection test needs and return its standard output.
fn run(program: &str, args: &[&str], gnupg: &std::path::Path) -> Result<Vec<u8>> {
    let output = std::process::Command::new(program)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C.UTF-8")
        .env("GNUPGHOME", gnupg)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(output.stdout)
}

fn collect(output: &std::path::Path) -> Result<std::process::Output> {
    Ok(
        std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .arg("collect-apt-provenance")
            .arg(output)
            .output()?,
    )
}

/// The collection rewrites the container's APT configuration and runs a real
/// `apt-get update`, so it only runs as root in a disposable Ubuntu 24.04
/// container with no network. There it serves a local repository signed by a
/// throwaway key and checks what was preserved: both indices with the signed
/// release that covers them, byte for byte; the Ubuntu archive keyring the
/// later authentication pins; source indices enabled in the existing
/// configuration; and an explicit collection time. A second collection on the
/// now used APT state is refused, and the preserved metadata is not
/// authenticated by the pinned Ubuntu key merely for having been collected.
#[test]
#[ignore = "rewrites APT configuration as root; exercised by the dedicated offline audit container step in Checks"]
fn actual_collection_preserves_a_local_signed_repository_in_a_root_container() -> Result<()> {
    if !std::path::Path::new("/.dockerenv").is_file() || !nix::unistd::geteuid().is_root() {
        return Err("this test must run as root in a disposable container".into());
    }
    let gnupg = std::path::Path::new("/root/among-fixture-gnupg");
    fs::create_dir(gnupg)?;
    fs::set_permissions(gnupg, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let architecture = String::from_utf8(run("/usr/bin/dpkg", &["--print-architecture"], gnupg)?)?
        .trim()
        .to_string();
    let repository = std::path::Path::new("/srv/among-apt-fixture");
    let binary = format!("main/binary-{architecture}/Packages");
    let source = "main/source/Sources".to_string();
    let packages = format!(
        "Package: fixture\nVersion: 1.0\nArchitecture: {architecture}\nFilename: pool/fixture.deb\nSize: 3\nSHA256: {}\n\n",
        "a".repeat(64)
    );
    let sources = format!(
        "Package: fixture\nVersion: 1.0\nChecksums-Sha256:\n {} 3 fixture.tar.xz\n\n",
        "b".repeat(64)
    );
    let dists = repository.join("dists/noble");
    let mut release = format!(
        "Origin: Ubuntu\nCodename: noble\nSuite: noble\nDate: Thu, 08 Oct 2026 23:00:00 UTC\n\
         Architectures: {architecture}\nComponents: main\nSHA256:\n"
    );
    for (member, text) in [(&binary, &packages), (&source, &sources)] {
        let path = dists.join(member);
        fs::create_dir_all(path.parent().ok_or("index parent absent")?)?;
        fs::write(&path, text)?;
        release.push_str(&format!(
            " {} {} {member}\n",
            storage::sha256(text.as_bytes())?,
            text.len()
        ));
    }
    fs::write(dists.join("Release"), &release)?;
    run(
        "/usr/bin/gpg",
        &[
            "--batch",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-gen-key",
            "Synthetic Fixture <fixture@example.invalid>",
            "ed25519",
            "sign",
            "never",
        ],
        gnupg,
    )?;
    let key = repository.join("fixture-key.gpg");
    fs::write(&key, run("/usr/bin/gpg", &["--batch", "--export"], gnupg)?)?;
    let signed = dists.join("InRelease");
    let release_path = dists.join("Release");
    run(
        "/usr/bin/gpg",
        &[
            "--batch",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--output",
            signed.to_str().ok_or("path not UTF-8")?,
            "--clearsign",
            release_path.to_str().ok_or("path not UTF-8")?,
        ],
        gnupg,
    )?;
    // Only the local repository may be configured: the container has no
    // network, and the image's own Ubuntu source would fail the update.
    let configuration = std::path::Path::new("/etc/apt/sources.list.d");
    for entry in fs::read_dir(configuration)? {
        fs::remove_file(entry?.path())?;
    }
    let fixture_sources = configuration.join("fixture.sources");
    fs::write(
        &fixture_sources,
        format!(
            "Types: deb\nURIs: file:{}\nSuites: noble\nComponents: main\nSigned-By: {}\n",
            repository.display(),
            key.display()
        ),
    )?;

    let output = std::path::Path::new("/root/among-apt-collected");
    let result = collect(output)?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        String::from_utf8(result.stdout)?,
        "Preserved 2 indices and signed metadata; cryptographic authentication remains a separate check\n"
    );
    let prefix = "_srv_among-apt-fixture_dists_noble";
    let document = storage::parse_json(&fs::read(output.join("inventory.json"))?)?;
    assert_eq!(document["schema"], 1);
    assert_eq!(
        document["indices"],
        json!([
            {"file": format!("{prefix}_main_binary-{architecture}_Packages"), "release": format!("{prefix}_InRelease"), "release_member": binary, "role": "binary"},
            {"file": format!("{prefix}_main_source_Sources"), "release": format!("{prefix}_InRelease"), "release_member": source, "role": "source"},
        ])
    );
    assert_eq!(
        fs::read_to_string(output.join(format!("{prefix}_main_binary-{architecture}_Packages")))?,
        packages
    );
    assert_eq!(
        fs::read_to_string(output.join(format!("{prefix}_main_source_Sources")))?,
        sources
    );
    assert_eq!(
        fs::read(output.join(format!("{prefix}_InRelease")))?,
        fs::read(&signed)?
    );
    assert_eq!(
        fs::read(output.join("archive-keyring.gpg"))?,
        fs::read("/usr/share/keyrings/ubuntu-archive-keyring.gpg")?
    );
    let collected = document["collected_at"]
        .as_str()
        .ok_or("collection time absent")?;
    assert!(
        collected.len() == 25 && collected.ends_with("+00:00") && &collected[10..11] == "T",
        "{collected}"
    );
    assert!(fs::read_to_string(&fixture_sources)?.contains("Types: deb deb-src\n"));

    let again = std::path::Path::new("/root/among-apt-collected-again");
    let repeated = collect(again)?;
    assert!(!repeated.status.success());
    assert_eq!(
        String::from_utf8(repeated.stderr)?.trim_end(),
        "among-tools: APT audit state must be fresh"
    );
    assert!(!again.exists());

    fs::write(
        "/root/native.json",
        serde_json::to_vec(&json!({"schema":1,"packages":[]}))?,
    )?;
    fs::create_dir("/root/sources")?;
    let authentication = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .env("AMONG_CHECK_ROOT", "/root/among-checks")
        .arg("check-apt-provenance")
        .arg(output)
        .arg("/root/native.json")
        .arg("/root/sources")
        .output()?;
    assert!(!authentication.status.success());
    assert_eq!(
        String::from_utf8(authentication.stderr)?.trim_end(),
        "among-tools: release is not signed by the pinned Ubuntu archive key"
    );
    Ok(())
}
