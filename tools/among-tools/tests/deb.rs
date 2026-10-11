use among_tools::{deb, storage, Result};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

struct Fixture(PathBuf);

#[test]
fn source_packaging_inputs_remain_locked_user_installer_free_and_unprivileged() -> Result<()> {
    let f = Fixture::new()?;
    let root = &f.0;
    fs::create_dir(root.join("packaging"))?;
    fs::create_dir(root.join("packaging/debian"))?;
    let rules="cargo +1.98.1 build --release --locked --bins\ncargo +1.98.1 build --release --locked --bins --manifest-path examples/plugins/Cargo.toml\n";
    fs::write(root.join("packaging/debian/rules"), rules)?;
    fs::write(
        root.join("packaging/debian/control"),
        "Maintainer: Among Terminal <hello+amongterminal@highload.zone>\nRules-Requires-Root: no\n",
    )?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='among'\nrepository='https://example.test/among'\npublish=false\n",
    )?;
    fs::write(
        root.join("README.md"),
        "cargo install --git https://example.test/among --locked among\ncargo uninstall among\n",
    )?;
    deb::check_source_inputs(root)?;
    for bad in [
        rules.replace("--locked", ""),
        format!("{rules}scripts/install.sh\n"),
        format!("{rules}install-plugins.sh\n"),
    ] {
        fs::write(root.join("packaging/debian/rules"), bad)?;
        assert!(deb::check_source_inputs(root).is_err());
    }
    fs::write(root.join("packaging/debian/rules"), rules)?;
    fs::write(
        root.join("README.md"),
        "cargo install --git wrong --locked among\ncargo uninstall among\n",
    )?;
    assert!(deb::check_source_inputs(root).is_err());
    Ok(())
}

#[test]
fn actual_dpkg_orders_the_ci_upgrade_fixture_before_current_version() -> Result<()> {
    let f = Fixture::new()?;
    for target in ["debian13", "ubuntu24.04"] {
        let version = deb::identity(&f.0, "v0.1.0", target, "amd64")?.package_version;
        let status = std::process::Command::new("/usr/bin/dpkg")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .args([
                "--compare-versions",
                &format!("{version}~ciolder"),
                "lt",
                &version,
            ])
            .status()?;
        assert!(status.success());
    }
    Ok(())
}

#[test]
fn desktop_exec_rewrite_covers_final_line_and_crlf_without_silent_noop() -> Result<()> {
    for entry in [
        "[Desktop Entry]\nExec=among",
        "[Desktop Entry]\r\nExec=among\r\n",
    ] {
        let f = Fixture::new()?;
        fs::write(f.0.join("data/com.github.among-terminal.desktop"), entry)?;
        let destination = f.0.join("staging");
        deb::stage(&f.0, &f.0.join("binaries"), &destination)?;
        assert_eq!(
            fs::read_to_string(
                destination.join("usr/share/applications/com.github.among-terminal.desktop")
            )?,
            entry.replace("Exec=among", "Exec=/usr/bin/among")
        );
    }
    Ok(())
}

#[test]
fn exact_candidate_source_target_digest_and_inventory_are_bound_before_install() -> Result<()> {
    let f = Fixture::new()?;
    let directory = f.0.join("candidate");
    fs::create_dir(&directory)?;
    let identity = deb::identity(&f.0, "v0.1.0", "debian13", "amd64")?;
    let path = directory.join(&identity.filename);
    fs::write(&path, b"fixture only, not a real deb")?;
    let mut record = serde_json::to_value(identity)?;
    record["tag"] = "v0.1.0".into();
    record["source_sha"] = "a".repeat(40).into();
    record["sha256"] = among_tools::storage::sha256(fs::File::open(&path)?)?.into();
    let metadata = directory.join("build-record.json");
    fs::write(&metadata, serde_json::to_vec(&record)?)?;
    assert_eq!(
        deb::validate_candidate(
            &f.0,
            &directory,
            "v0.1.0",
            &"a".repeat(40),
            "debian13",
            "amd64"
        )?
        .0,
        path
    );
    for (target, arch, source) in [
        ("ubuntu24.04", "amd64", 'a'),
        ("debian13", "arm64", 'a'),
        ("debian13", "amd64", 'b'),
    ] {
        assert!(deb::validate_candidate(
            &f.0,
            &directory,
            "v0.1.0",
            &source.to_string().repeat(40),
            target,
            arch
        )
        .is_err());
    }
    fs::write(directory.join("extra.deb"), b"extra")?;
    assert!(deb::validate_candidate(
        &f.0,
        &directory,
        "v0.1.0",
        &"a".repeat(40),
        "debian13",
        "amd64"
    )
    .is_err());
    fs::remove_file(directory.join("extra.deb"))?;
    fs::write(&path, b"changed")?;
    assert!(deb::validate_candidate(
        &f.0,
        &directory,
        "v0.1.0",
        &"a".repeat(40),
        "debian13",
        "amd64"
    )
    .err()
    .is_some_and(|error| error.to_string().contains("digest changed")));
    fs::remove_file(&path)?;
    std::os::unix::fs::symlink("/bin/true", &path)?;
    assert!(deb::validate_candidate(
        &f.0,
        &directory,
        "v0.1.0",
        &"a".repeat(40),
        "debian13",
        "amd64"
    )
    .is_err());
    fs::write(metadata, b"{\"sha256\":\"a\",\"sha256\":\"b\"}")?;
    assert!(deb::validate_candidate(
        &f.0,
        &directory,
        "v0.1.0",
        &"a".repeat(40),
        "debian13",
        "amd64"
    )
    .is_err());
    Ok(())
}

#[test]
fn runtime_policy_rejects_build_dependencies_headers_and_hidden_commands() -> Result<()> {
    for name in [
        "cargo",
        "rustc",
        "rustup",
        "build-essential",
        "libgtk-4-dev:arm64",
        "python3-gi",
        "python3-gi-cairo:amd64",
        "python3-cairo",
    ] {
        assert!(deb::runtime_inventory_only(&format!("{name}\tinstalled\n"), &[]).is_err());
    }
    deb::runtime_inventory_only("bash\tinstalled\nlibgtk-4-dev\tconfig-files\n", &[])?;
    for command in ["cargo", "rustc", "rustup"] {
        assert!(deb::runtime_inventory_only("bash\tinstalled\n", &[command]).is_err());
    }
    assert!(deb::runtime_inventory_only("broken row", &[]).is_err());
    Ok(())
}

#[test]
fn lifecycle_hash_guard_observes_changed_and_missing_user_state() -> Result<()> {
    let f = Fixture::new()?;
    let path = f.0.join("user-state");
    fs::write(&path, b"preferences-before")?;
    let paths = vec![path.clone()];
    let original = deb::state_hashes(&paths)?;
    deb::require_unchanged(&paths, &original)?;
    fs::write(&path, b"preferences-after")?;
    assert!(deb::require_unchanged(&paths, &original).is_err());
    fs::remove_file(path)?;
    assert!(deb::require_unchanged(&paths, &original).is_err());
    Ok(())
}

#[test]
fn selected_payload_rejects_changed_modes_bytes_missing_extra_and_symlink_files() -> Result<()> {
    let f = Fixture::new()?;
    let staged = f.0.join("staged");
    let selected = f.0.join("selected");
    fs::create_dir(&staged)?;
    fs::create_dir(&selected)?;
    for root in [&staged, &selected] {
        fs::create_dir(root.join("usr"))?;
        fs::write(root.join("usr/payload"), b"same bytes")?;
        fs::set_permissions(root.join("usr/payload"), fs::Permissions::from_mode(0o644))?;
    }
    assert_eq!(
        among_tools::deb_selection::require_equal(&staged, &selected)?,
        1
    );
    let payload = selected.join("usr/payload");
    fs::write(&payload, b"changed")?;
    assert!(among_tools::deb_selection::require_equal(&staged, &selected).is_err());
    fs::write(&payload, b"same bytes")?;
    fs::set_permissions(&payload, fs::Permissions::from_mode(0o755))?;
    assert!(among_tools::deb_selection::require_equal(&staged, &selected).is_err());
    fs::set_permissions(&payload, fs::Permissions::from_mode(0o644))?;
    fs::write(selected.join("usr/extra"), b"unapproved")?;
    assert!(among_tools::deb_selection::require_equal(&staged, &selected).is_err());
    fs::remove_file(selected.join("usr/extra"))?;
    fs::remove_file(&payload)?;
    assert!(among_tools::deb_selection::require_equal(&staged, &selected).is_err());
    std::os::unix::fs::symlink(staged.join("usr/payload"), &payload)?;
    assert!(among_tools::deb_selection::require_equal(&staged, &selected).is_err());
    Ok(())
}
impl Fixture {
    fn new() -> Result<Self> {
        let base = storage::private_directory(&std::env::temp_dir(), "among-deb-fixture")?;
        for name in [
            "data",
            "examples",
            "examples/plugins",
            "THIRD_PARTY",
            "binaries",
            "home",
        ] {
            fs::create_dir(base.join(name))?;
        }
        for path in ["Cargo.toml", "examples/plugins/Cargo.toml"] {
            fs::write(
                base.join(path),
                "[package]\nname=\"fixture\"\nversion=\"0.1.0\"\n",
            )?;
        }
        fs::write(
            base.join("data/com.github.among-terminal.metainfo.xml"),
            "<component><releases><release version=\"0.1.0\"/></releases></component>",
        )?;
        fs::write(base.join("data/com.github.among-terminal.desktop"),"[Desktop Entry]\nType=Application\nName=Among Terminal\nExec=among\nIcon=com.github.among-terminal\nStartupWMClass=com.github.among-terminal\n")?;
        fs::write(
            base.join("data/com.github.among-terminal.svg"),
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"128\" height=\"128\"/>",
        )?;
        for name in [
            "LICENSE",
            "NOTICE",
            "THIRD_PARTY/inventory.json",
            "THIRD_PARTY/license.txt",
        ] {
            fs::write(base.join(name), format!("fixture {name}"))?;
        }
        for name in deb::EXECUTABLES {
            fs::copy("/bin/true", base.join("binaries").join(name))?;
            fs::set_permissions(
                base.join("binaries").join(name),
                fs::Permissions::from_mode(0o755),
            )?;
        }
        Ok(Self(base))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn real_tag_commit_and_main_ancestry_require_release_push_identity() -> Result<()> {
    let f = Fixture::new()?;
    let git = |args: &[&str]| -> Result<String> {
        let output = std::process::Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .arg("-C")
            .arg(&f.0)
            .args(args)
            .output()?;
        if !output.status.success() {
            return Err("fixture Git operation failed".into());
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_string())
    };
    git(&["init", "-q", "-b", "main"])?;
    git(&["add", "."])?;
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "fixture",
    ])?;
    git(&["tag", "v0.1.0"])?;
    let source = git(&["rev-parse", "HEAD"])?;
    git(&["update-ref", "refs/remotes/origin/main", &source])?;
    let mut environment = std::collections::BTreeMap::from([
        ("GITHUB_ACTIONS".into(), "true".into()),
        ("GITHUB_EVENT_NAME".into(), "push".into()),
        ("GITHUB_REF".into(), "refs/tags/v0.1.0".into()),
        ("GITHUB_SHA".into(), source.clone()),
    ]);
    assert_eq!(
        deb::github_tag_source(&f.0, &environment)?,
        ("v0.1.0".into(), source.clone())
    );
    environment.insert("GITHUB_SHA".into(), "0".repeat(40));
    assert!(deb::github_tag_source(&f.0, &environment).is_err());
    environment.insert("GITHUB_SHA".into(), source.clone());
    for (event, reference) in [
        ("pull_request", "refs/pull/1/merge"),
        ("schedule", "refs/heads/main"),
        ("push", "refs/heads/main"),
    ] {
        environment.insert("GITHUB_EVENT_NAME".into(), event.into());
        environment.insert("GITHUB_REF".into(), reference.into());
        assert!(deb::github_tag_source(&f.0, &environment).is_err());
    }
    environment.insert("GITHUB_EVENT_NAME".into(), "push".into());
    environment.insert("GITHUB_REF".into(), "refs/tags/v0.1.0".into());
    git(&["checkout", "--orphan", "unrelated"])?;
    git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "commit",
        "-qm",
        "unrelated",
    ])?;
    let unrelated = git(&["rev-parse", "HEAD"])?;
    git(&["update-ref", "refs/remotes/origin/main", &unrelated])?;
    git(&["checkout", "main"])?;
    assert!(deb::github_tag_source(&f.0, &environment).is_err());
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .args([
            "deb-tag-source",
            f.0.to_str().ok_or("fixture root not UTF-8")?,
        ])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains("restricted to GitHub release-tag push CI"));
    Ok(())
}

#[test]
fn four_native_identities_and_all_original_stable_version_refusals() -> Result<()> {
    let f = Fixture::new()?;
    let mut records = Vec::new();
    for target in ["debian13", "ubuntu24.04"] {
        for arch in ["amd64", "arm64"] {
            let record = deb::identity(&f.0, "v0.1.0", target, arch)?;
            assert_eq!(record.upstream_version, "0.1.0");
            assert!(record.package_version.contains(target));
            assert!(record.filename.ends_with(&format!("_{arch}.deb")));
            records.push(record);
        }
    }
    deb::unique_inventory(&records)?;
    assert!(deb::unique_inventory(&[records[0].clone(), records[0].clone()]).is_err());
    let mut other = records[3].clone();
    other.filename = records[0].filename.clone();
    assert!(deb::unique_inventory(&[records[0].clone(), other]).is_err());
    for tag in [
        "main",
        "v01.0.0",
        "v0.1.0-rc1",
        "v0.1.0+build",
        "nightly",
        "v0.1",
        "v0.1.0;false",
    ] {
        assert!(deb::identity(&f.0, tag, "debian13", "amd64").is_err());
    }
    for (target, arch) in [
        ("debian12", "amd64"),
        ("ubuntu22.04", "amd64"),
        ("debian13", "i386"),
        ("debian13", "x32"),
    ] {
        assert!(deb::identity(&f.0, "v0.1.0", target, arch).is_err());
    }
    Ok(())
}
#[test]
fn both_manifests_metadata_and_elf_targets_are_checked() -> Result<()> {
    let f = Fixture::new()?;
    for path in [
        "Cargo.toml",
        "examples/plugins/Cargo.toml",
        "data/com.github.among-terminal.metainfo.xml",
    ] {
        let path = f.0.join(path);
        let before = fs::read_to_string(&path)?;
        fs::write(&path, before.replace("0.1.0", "0.2.0"))?;
        assert!(deb::identity(&f.0, "v0.1.0", "debian13", "amd64").is_err());
        fs::write(path, before)?;
    }
    let path = f.0.join("elf-fixture");
    for (arch, machine, other) in [("amd64", 62u16, "arm64"), ("arm64", 183u16, "amd64")] {
        let mut bytes = [0u8; 20];
        bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        fs::write(&path, bytes)?;
        deb::elf_architecture(&path, arch)?;
        assert!(deb::elf_architecture(&path, other).is_err());
    }
    fs::write(&path, b"not an ELF")?;
    assert!(deb::elf_architecture(&path, "amd64").is_err());
    fs::write(
        f.0.join("Cargo.toml"),
        "[package]\nversion='0.1.0'\nversion='0.1.0'\n",
    )?;
    assert!(deb::identity(&f.0, "v0.1.0", "debian13", "amd64").is_err());
    Ok(())
}
#[test]
fn stage_exact_payload_modes_attribution_and_desktop_without_user_installer_state() -> Result<()> {
    let f = Fixture::new()?;
    let destination = f.0.join("staging");
    assert_eq!(deb::stage(&f.0, &f.0.join("binaries"), &destination)?, 11);
    for name in deb::EXECUTABLES {
        let path = destination.join(if name == "among" {
            "usr/bin/among".into()
        } else {
            format!("usr/share/among/plugins/{name}")
        });
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o755);
        assert_eq!(fs::read(path)?, fs::read(f.0.join("binaries").join(name))?);
    }
    for name in ["inventory.json", "license.txt"] {
        let path = destination
            .join("usr/share/doc/among-terminal/THIRD_PARTY")
            .join(name);
        assert_eq!(
            fs::read(&path)?,
            fs::read(f.0.join("THIRD_PARTY").join(name))?
        );
        assert_eq!(fs::metadata(path)?.permissions().mode() & 0o777, 0o644);
    }
    let entry = destination.join("usr/share/applications/com.github.among-terminal.desktop");
    assert!(fs::read_to_string(&entry)?.contains("Exec=/usr/bin/among\n"));
    assert_eq!(fs::metadata(entry)?.permissions().mode() & 0o777, 0o644);
    assert!(!destination.join("home").exists());
    assert!(!destination.join("postinst").exists());
    assert!(!destination.join("install.json").exists());
    fs::write(destination.join("user-file"), b"keep")?;
    assert!(deb::stage(&f.0, &f.0.join("binaries"), &destination).is_err());
    assert_eq!(fs::read(destination.join("user-file"))?, b"keep");
    Ok(())
}
#[test]
fn staging_refuses_missing_nonexec_empty_symlink_and_attribution_links_before_output() -> Result<()>
{
    for failure in [
        "missing",
        "nonexec",
        "empty",
        "symlink",
        "attribution",
        "inventory",
    ] {
        let f = Fixture::new()?;
        let binary = f.0.join("binaries/among");
        match failure {
            "missing" => fs::remove_file(f.0.join("binaries/clock"))?,
            "nonexec" => fs::set_permissions(&binary, fs::Permissions::from_mode(0o644))?,
            "empty" => fs::write(&binary, b"")?,
            "symlink" => {
                fs::remove_file(&binary)?;
                std::os::unix::fs::symlink("/bin/true", &binary)?;
            }
            "attribution" => std::os::unix::fs::symlink("/", f.0.join("THIRD_PARTY/foreign"))?,
            "inventory" => fs::remove_file(f.0.join("THIRD_PARTY/inventory.json"))?,
            _ => return Err("unknown fixture case".into()),
        }
        let destination = f.0.join("staging");
        assert!(deb::stage(&f.0, &f.0.join("binaries"), &destination).is_err());
        assert!(!destination.exists());
    }
    Ok(())
}
#[test]
fn actual_staging_cli_does_not_write_synthetic_home() -> Result<()> {
    let f = Fixture::new()?;
    fs::write(f.0.join("home/.zshrc"), b"keep")?;
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .env("HOME", f.0.join("home"))
        .env("XDG_DATA_HOME", f.0.join("home/data"))
        .env("XDG_CONFIG_HOME", f.0.join("home/config"))
        .arg("stage-deb")
        .arg(&f.0)
        .arg(f.0.join("binaries"))
        .arg(f.0.join("staging"))
        .output()?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(fs::read_dir(f.0.join("home"))?.count(), 1);
    assert_eq!(fs::read(f.0.join("home/.zshrc"))?, b"keep");
    Ok(())
}
