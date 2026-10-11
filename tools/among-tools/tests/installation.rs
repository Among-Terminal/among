use among_tools::{
    installation::{Layout, Manifest, Record},
    storage,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

struct Fixture(PathBuf);

fn native_payload() -> among_tools::Result<BTreeMap<String, among_tools::installation::Payload>> {
    use among_tools::installation::Payload;
    [
        "binary",
        "desktop",
        "icon",
        "metadata",
        "license",
        "notice",
        "uninstaller",
    ]
    .into_iter()
    .map(|key| {
        Ok((
            key.into(),
            Payload {
                bytes: if key == "uninstaller" {
                    fs::read(env!("CARGO_BIN_EXE_among-uninstall"))?
                } else {
                    format!("new-{key}").into_bytes()
                },
                mode: if matches!(key, "binary" | "uninstaller") {
                    0o755
                } else {
                    0o644
                },
            },
        ))
    })
    .collect()
}

#[test]
fn foreign_file_and_invalid_binary_refuse_install_without_partial_payload(
) -> among_tools::Result<()> {
    let fixture = Fixture::new()?;
    let layout = fixture.layout()?;
    let icon = &layout.destinations["icon"];
    fs::create_dir_all(icon.parent().ok_or("icon parent missing")?)?;
    fs::write(icon, b"user-icon")?;
    assert!(
        among_tools::installation::install_payload(fixture.layout()?, native_payload()?).is_err()
    );
    assert_eq!(fs::read(icon)?, b"user-icon");
    assert!(!layout.destinations["binary"].exists());
    fs::remove_file(icon)?;
    among_tools::installation::install_payload(fixture.layout()?, native_payload()?)?;
    let before = fs::read(layout.state.join("install.json"))?;
    let mut empty = native_payload()?;
    empty
        .get_mut("binary")
        .ok_or("binary missing")?
        .bytes
        .clear();
    assert!(among_tools::installation::install_payload(fixture.layout()?, empty).is_err());
    assert_eq!(fs::read(&layout.destinations["binary"])?, b"new-binary");
    assert_eq!(fs::read(layout.state.join("install.json"))?, before);
    Ok(())
}

#[test]
fn missing_owned_file_does_not_block_removal_and_empty_removal_is_idempotent(
) -> among_tools::Result<()> {
    let fixture = Fixture::new()?;
    let manifest = prepare(&fixture)?;
    fs::remove_file(&manifest.files["binary"].path)?;
    among_tools::installation::uninstall(fixture.layout()?)?;
    assert!(!manifest.files["uninstaller"].path.exists());
    assert!(!fixture.layout()?.state.join("install.json").exists());
    among_tools::installation::uninstall(fixture.layout()?)?;
    Ok(())
}

#[test]
fn dependency_install_is_explicit_and_build_failure_never_replaces_owned_files(
) -> among_tools::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new()?;
    let manifest = prepare(&fixture)?;
    let before = fs::read(fixture.layout()?.state.join("install.json"))?;
    let commands = fixture.0.join("commands");
    fs::create_dir(&commands)?;
    for (name,body) in [
        ("sudo","#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HOME/dependencies.log\"\nexec \"$@\"\n"),
        ("apt-get","#!/bin/sh\nexit 0\n"),
        ("pkg-config","#!/bin/sh\nexit 0\n"),
        ("cargo","#!/bin/sh\n/usr/bin/id -u > \"$HOME/build-uid\"\nprintf '%s\\n' \"$*\" > \"$HOME/build-args\"\nexit 19\n")
    ] {
        fs::write(commands.join(name),body)?;
        fs::set_permissions(commands.join(name),fs::Permissions::from_mode(0o755))?;
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .env("HOME", fixture.0.join("home"))
        .env("XDG_DATA_HOME", fixture.0.join("data"))
        .env("XDG_STATE_HOME", fixture.0.join("state"))
        .env("PATH", &commands)
        .arg("install")
        .arg(&fixture.0)
        .arg("--install-deps")
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains("external installer operation failed"));
    assert_eq!(fs::read(&manifest.files["binary"].path)?, b"owned-binary");
    assert_eq!(
        fs::read(fixture.layout()?.state.join("install.json"))?,
        before
    );
    assert_eq!(
        fs::read_to_string(fixture.0.join("home/build-uid"))?.trim(),
        nix::unistd::geteuid().to_string()
    );
    let arguments = fs::read_to_string(fixture.0.join("home/build-args"))?;
    assert!(arguments.contains("--locked --root"));
    assert!(arguments.contains("--target-dir"));
    let dependencies = fs::read_to_string(fixture.0.join("home/dependencies.log"))?;
    assert_eq!(dependencies.lines().count(), 2);
    assert!(dependencies.contains(" install -y build-essential pkg-config libgtk-4-dev libadwaita-1-dev libvte-2.91-gtk4-dev desktop-file-utils libgtk-4-bin librsvg2-common"));
    Ok(())
}

#[test]
fn preserved_install_options_refuse_conflicts_and_missing_values() -> among_tools::Result<()> {
    use among_tools::installation::options;
    for args in [
        vec!["--binary"],
        vec!["--binary", "a", "--install-deps"],
        vec!["--uninstall", "--binary", "a"],
        vec!["--uninstall", "--install-deps"],
        vec!["--install-deps", "--install-deps"],
        vec!["--unknown"],
    ] {
        assert!(options(&args.into_iter().map(str::to_string).collect::<Vec<_>>()).is_err());
    }
    assert!(options(&["--install-deps".into()])?.install_deps);
    assert!(options(&["--uninstall".into()])?.uninstall);
    assert_eq!(
        options(&["--binary".into(), "fixture".into()])?.binary,
        Some("fixture".into())
    );
    Ok(())
}

#[test]
fn source_dependency_failure_preserves_existing_native_payload_and_manifest(
) -> among_tools::Result<()> {
    let fixture = Fixture::new()?;
    let manifest = prepare(&fixture)?;
    let old_manifest = fs::read(fixture.layout()?.state.join("install.json"))?;
    for args in [vec![], vec!["--install-deps"]] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .env("HOME", fixture.0.join("home"))
            .env("XDG_DATA_HOME", fixture.0.join("data"))
            .env("XDG_STATE_HOME", fixture.0.join("state"))
            .env("PATH", "/no-commands")
            .arg("install")
            .arg(&fixture.0)
            .args(args)
            .output()?;
        assert!(!output.status.success());
        assert_eq!(fs::read(&manifest.files["binary"].path)?, b"owned-binary");
        assert_eq!(
            fs::read(fixture.layout()?.state.join("install.json"))?,
            old_manifest
        );
    }
    Ok(())
}

#[test]
fn actual_v1_python_launcher_migration_preserves_user_files_and_refuses_altered_helper(
) -> among_tools::Result<()> {
    use among_tools::installation::{install_payload, Payload};
    let fixture = Fixture::new()?;
    let mut manifest = prepare(&fixture)?;
    for (key, bytes) in [
        (
            "installer",
            b"#!/usr/bin/env python3\nprint('legacy fixture')\n".as_slice(),
        ),
        (
            "uninstaller",
            b"#!/bin/sh\nexec python3 legacy/install.py --uninstall \"$@\"\n".as_slice(),
        ),
    ] {
        fs::write(&manifest.files[key].path, bytes)?;
        manifest.files.get_mut(key).ok_or("missing record")?.sha256 = storage::sha256(bytes)?;
    }
    fs::write(
        fixture.layout()?.state.join("install.json"),
        serde_json::to_vec(&manifest)?,
    )?;
    let payload = || -> among_tools::Result<BTreeMap<String, Payload>> {
        [
            "binary",
            "desktop",
            "icon",
            "metadata",
            "license",
            "notice",
            "uninstaller",
        ]
        .into_iter()
        .map(|key| {
            Ok((
                key.into(),
                Payload {
                    bytes: if key == "uninstaller" {
                        fs::read(env!("CARGO_BIN_EXE_among-uninstall"))?
                    } else {
                        b"new-owned".to_vec()
                    },
                    mode: if matches!(key, "binary" | "uninstaller") {
                        0o755
                    } else {
                        0o644
                    },
                },
            ))
        })
        .collect()
    };
    for name in [
        "config/among/plugins.json",
        "data/among/session.json",
        "data/among/plugins/external",
    ] {
        let path = fixture.0.join(name);
        fs::create_dir_all(path.parent().ok_or("user state has no parent")?)?;
        fs::write(path, b"personal-state")?;
    }
    let shell = fixture.0.join("home/.bashrc");
    fs::write(&shell, b"personal-shell-config")?;
    fs::write(
        &manifest.files["installer"].path,
        b"user-modified legacy helper",
    )?;
    assert!(install_payload(fixture.layout()?, payload()?).is_err());
    assert!(fs::read(&manifest.files["uninstaller"].path)?.starts_with(b"#!/bin/sh"));
    fs::write(
        &manifest.files["installer"].path,
        b"#!/usr/bin/env python3\nprint('legacy fixture')\n",
    )?;
    install_payload(fixture.layout()?, payload()?)?;
    assert!(!manifest.files["installer"].path.exists());
    assert!(fs::read(&manifest.files["uninstaller"].path)?.starts_with(b"\x7fELF"));
    assert_eq!(fs::read(shell)?, b"personal-shell-config");
    for name in [
        "config/among/plugins.json",
        "data/among/session.json",
        "data/among/plugins/external",
    ] {
        assert_eq!(fs::read(fixture.0.join(name))?, b"personal-state");
    }
    Ok(())
}
#[test]
#[ignore = "requires disposable container with GIO and an owned display"]
fn actual_gio_launch_decodes_special_character_home_without_shell_expansion(
) -> among_tools::Result<()> {
    let fixture = Fixture::new()?;
    let binary = fixture.0.join("home space % \" $ ` \\").join("among");
    fs::create_dir_all(binary.parent().ok_or("missing binary parent")?)?;
    fs::copy("/bin/true", &binary)?;
    let desktop = fixture.0.join("among.desktop");
    fs::write(
        &desktop,
        among_tools::installation::desktop_content(
            "[Desktop Entry]\nType=Application\nName=Among Terminal\nExec=among\nTerminal=false\n",
            &binary,
        )?,
    )?;
    let output = std::process::Command::new("/usr/bin/gio")
        .arg("launch")
        .arg(&desktop)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
impl Fixture {
    fn new() -> among_tools::Result<Self> {
        let path = storage::private_directory(&std::env::temp_dir(), "among-uninstall-fixture")?;
        Ok(Self(path))
    }
    fn layout(&self) -> among_tools::Result<Layout> {
        Layout::new(
            &self.0.join("home"),
            &self.0.join("data"),
            &self.0.join("state"),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn prepare(fixture: &Fixture) -> among_tools::Result<Manifest> {
    let layout = fixture.layout()?;
    let mut files = BTreeMap::new();
    for key in ["binary", "installer", "uninstaller"] {
        let path = layout.destinations[key].clone();
        fs::create_dir_all(path.parent().ok_or("missing fixture parent")?)?;
        let bytes = if key == "uninstaller" {
            fs::read(env!("CARGO_BIN_EXE_among-uninstall"))?
        } else {
            format!("owned-{key}").into_bytes()
        };
        fs::write(&path, &bytes)?;
        files.insert(
            key.into(),
            Record {
                path,
                sha256: storage::sha256(bytes.as_slice())?,
            },
        );
    }
    let manifest = Manifest { version: 1, files };
    fs::create_dir_all(&layout.state)?;
    fs::write(
        layout.state.join("install.json"),
        serde_json::to_vec(&manifest)?,
    )?;
    Ok(manifest)
}

#[test]
fn standalone_removal_without_checkout_cargo_or_interpreter_preserves_user_state(
) -> among_tools::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new()?;
    let manifest = prepare(&fixture)?;
    let executable = &manifest.files["uninstaller"].path;
    fs::set_permissions(executable, fs::Permissions::from_mode(0o755))?;
    for name in [
        "config/among/plugins.json",
        "data/among/session.json",
        "data/among/plugins/external",
        "home/.bashrc",
    ] {
        let path = fixture.0.join(name);
        fs::create_dir_all(path.parent().ok_or("missing state parent")?)?;
        fs::write(path, b"user-owned")?;
    }
    let output = std::process::Command::new(executable)
        .env_clear()
        .env("PATH", "/no-commands")
        .env("HOME", fixture.0.join("home"))
        .env("XDG_DATA_HOME", fixture.0.join("data"))
        .env("XDG_STATE_HOME", fixture.0.join("state"))
        .current_dir(&fixture.0)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for record in manifest.files.values() {
        assert!(!record.path.exists());
    }
    for name in [
        "config/among/plugins.json",
        "data/among/session.json",
        "data/among/plugins/external",
        "home/.bashrc",
    ] {
        assert_eq!(fs::read(fixture.0.join(name))?, b"user-owned");
    }
    assert!(!fixture.layout()?.state.join("install.json").exists());
    Ok(())
}

#[test]
fn modified_files_symlinks_and_concurrent_operations_are_refused_before_removal(
) -> among_tools::Result<()> {
    let fixture = Fixture::new()?;
    let manifest = prepare(&fixture)?;
    fs::write(&manifest.files["binary"].path, b"modified")?;
    assert!(among_tools::installation::uninstall(fixture.layout()?).is_err());
    assert!(manifest.files["installer"].path.exists());
    fs::write(&manifest.files["binary"].path, b"owned-binary")?;
    let layout = fixture.layout()?;
    let lock = storage::ExclusiveLock::acquire(&layout.state, std::path::Path::new("lock"))?;
    assert!(among_tools::installation::uninstall(fixture.layout()?).is_err());
    assert!(manifest.files["binary"].path.exists());
    drop(lock);
    fs::remove_file(&manifest.files["binary"].path)?;
    std::os::unix::fs::symlink("/", &manifest.files["binary"].path)?;
    assert!(among_tools::installation::uninstall(fixture.layout()?).is_err());
    assert!(manifest.files["installer"].path.exists());
    Ok(())
}

#[test]
fn manifest_cannot_authorize_unknown_keys_changed_layout_or_traversal() -> among_tools::Result<()> {
    let fixture = Fixture::new()?;
    let manifest = prepare(&fixture)?;
    for (key, path) in [
        ("unknown", fixture.0.join("outside")),
        ("binary", fixture.0.join("outside")),
        ("THIRD_PARTY/../../outside", fixture.0.join("outside")),
    ] {
        let modified = Manifest {
            version: 1,
            files: BTreeMap::from([(
                key.into(),
                Record {
                    path,
                    sha256: "a".repeat(64),
                },
            )]),
        };
        fs::write(
            fixture.layout()?.state.join("install.json"),
            serde_json::to_vec(&modified)?,
        )?;
        assert!(fixture.layout()?.read_manifest().is_err());
        assert!(manifest.files["binary"].path.exists());
    }
    Ok(())
}

#[test]
fn native_payload_upgrade_retires_legacy_helper_and_rewrites_owned_manifest(
) -> among_tools::Result<()> {
    use among_tools::installation::{install_payload, Payload};
    let fixture = Fixture::new()?;
    let old = prepare(&fixture)?;
    let payload = [
        "binary",
        "desktop",
        "icon",
        "metadata",
        "license",
        "notice",
        "uninstaller",
    ]
    .into_iter()
    .map(|key| {
        Ok((
            key.to_string(),
            Payload {
                bytes: if key == "uninstaller" {
                    fs::read(env!("CARGO_BIN_EXE_among-uninstall"))?
                } else {
                    format!("new-{key}").into_bytes()
                },
                mode: if matches!(key, "binary" | "uninstaller") {
                    0o755
                } else {
                    0o644
                },
            },
        ))
    })
    .collect::<among_tools::Result<BTreeMap<_, _>>>()?;
    install_payload(fixture.layout()?, payload)?;
    assert!(!old.files["installer"].path.exists());
    assert_eq!(fs::read(&old.files["binary"].path)?, b"new-binary");
    let mut layout = fixture.layout()?;
    let manifest = layout.read_manifest()?.ok_or("new manifest missing")?;
    assert_eq!(manifest.files.len(), 7);
    assert!(!manifest.files.contains_key("installer"));
    layout.check_owned(&manifest.files)?;
    among_tools::installation::uninstall(layout)?;
    for record in manifest.files.values() {
        assert!(!record.path.exists());
    }
    Ok(())
}

#[test]
fn actual_prebuilt_cli_installs_updates_and_uninstalls_without_external_commands(
) -> among_tools::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new()?;
    let source = fixture.0.join("source");
    fs::create_dir_all(source.join("data"))?;
    fs::create_dir_all(source.join("THIRD_PARTY/nested"))?;
    for name in [
        "LICENSE",
        "NOTICE",
        "THIRD_PARTY/nested/COPYING",
        "data/com.github.among-terminal.svg",
        "data/com.github.among-terminal.metainfo.xml",
    ] {
        fs::write(source.join(name), format!("fixture-{name}"))?;
    }
    fs::write(
        source.join("data/com.github.among-terminal.desktop"),
        "[Desktop Entry]\nType=Application\nName=Among Terminal\nExec=among\n",
    )?;
    let binary = fixture.0.join("prebuilt");
    fs::copy(env!("CARGO_BIN_EXE_among-uninstall"), &binary)?;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
    let invoke = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .env("PATH", "/no-commands")
            .env("HOME", fixture.0.join("home"))
            .env("XDG_DATA_HOME", fixture.0.join("data"))
            .env("XDG_STATE_HOME", fixture.0.join("state"))
            .args(["install-prebuilt"])
            .arg(&source)
            .arg(&binary)
            .arg(env!("CARGO_BIN_EXE_among-uninstall"))
            .output()
    };
    let output = invoke()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut layout = fixture.layout()?;
    let manifest = layout.read_manifest()?.ok_or("manifest missing")?;
    layout.check_owned(&manifest.files)?;
    assert_eq!(manifest.files.len(), 8);
    assert!(!layout.destinations["installer"].exists());
    fs::remove_file(source.join("THIRD_PARTY/nested/COPYING"))?;
    assert!(invoke()?.status.success());
    assert!(!layout
        .documentation
        .join("THIRD_PARTY/nested/COPYING")
        .exists());
    fs::write(&layout.destinations["binary"], b"user-modified")?;
    assert!(!invoke()?.status.success());
    assert_eq!(fs::read(&layout.destinations["binary"])?, b"user-modified");
    fs::copy(&binary, &layout.destinations["binary"])?;
    let output = std::process::Command::new(&layout.destinations["uninstaller"])
        .env_clear()
        .env("HOME", fixture.0.join("home"))
        .env("XDG_DATA_HOME", fixture.0.join("data"))
        .env("XDG_STATE_HOME", fixture.0.join("state"))
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!layout.destinations["uninstaller"].exists());
    Ok(())
}

#[test]
fn desktop_quote_retains_percent_field_codes_and_refuses_ambiguous_templates(
) -> among_tools::Result<()> {
    use among_tools::installation::desktop_content;
    let template = "[Desktop Entry]\nExec=among\n";
    let content = String::from_utf8(desktop_content(
        template,
        std::path::Path::new("/home/test space%/among"),
    )?)?;
    assert!(content.contains("Exec=/usr/bin/env \"/home/test space%%/among\""));
    assert!(desktop_content(template, std::path::Path::new("/home/test\n/among")).is_err());
    assert!(desktop_content("Exec=a\nExec=b\n", std::path::Path::new("/among")).is_err());
    Ok(())
}
