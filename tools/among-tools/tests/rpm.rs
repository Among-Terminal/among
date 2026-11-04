use among_tools::{package_version, rpm, storage, Result};
use std::fs;

#[test]
fn release_spec_uses_real_rc_ordering_and_refuses_manifest_mismatch() -> Result<()> {
    let root = fixture()?;
    for (tag, expected) in [("v0.1.0", "0.1.0"), ("v0.1.0-rc.3", "0.1.0~rc.3")] {
        let (version, text) = rpm::release_spec(&root, tag)?;
        assert_eq!(version, expected);
        assert!(text.contains(&format!("Version:        {expected}\n")));
        assert!(text.contains("Source0:        %{name}-%{version}.tar.gz"));
    }
    for tag in ["v0.2.0", "v0.1.0-rc.0", "v0.1.0-rc.01", "v0.1.0-rc.1\n"] {
        assert!(rpm::release_spec(&root, tag).is_err());
    }
    let original = spec_of(&root);
    write_spec(
        &root,
        &original.replace(
            "s|^Exec=among$|Exec=/usr/bin/among|",
            "s|^Icon=com.github.among-terminal$|Icon=usr/share/icon.svg|",
        ),
    )?;
    assert!(rpm::release_spec(&root, "v0.1.0").is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}

fn fixture() -> Result<std::path::PathBuf> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-rpm")?;
    fs::create_dir_all(root.join("packaging/rpm"))?;
    fs::create_dir_all(root.join("data"))?;
    // The real root manifest: its binding features are what the spec floors are
    // checked against, so a synthetic copy could drift from the real levels.
    // pin only the fixture version; retain the real binding features as the
    // independent source of library floors when the product version advances.
    let mut manifest: toml::Value = toml::from_str(include_str!("../../../Cargo.toml"))?;
    manifest["package"]["version"] = toml::Value::String("0.1.0".into());
    fs::write(root.join("Cargo.toml"), toml::to_string(&manifest)?)?;
    fs::create_dir_all(root.join("THIRD_PARTY"))?;
    fs::write(root.join("LICENSE"), "synthetic license\n")?;
    fs::write(root.join("NOTICE"), "synthetic notice\n")?;
    fs::write(root.join("THIRD_PARTY/inventory.json"), "{}\n")?;
    fs::write(
        root.join("data/com.github.among-terminal.desktop"),
        "[Desktop Entry]\nIcon=com.github.among-terminal\n",
    )?;
    fs::write(
        root.join("data/com.github.among-terminal.metainfo.xml"),
        "<component><id>com.github.among-terminal</id></component>\n",
    )?;
    let spec = include_str!("../../../packaging/rpm/among.spec")
        .lines()
        .map(|line| {
            if line.starts_with("Version:") {
                "Version:        0.1.0".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(root.join("packaging/rpm/among.spec"), format!("{spec}\n"))?;
    Ok(root)
}

fn spec_of(root: &std::path::Path) -> String {
    fs::read_to_string(root.join("packaging/rpm/among.spec")).unwrap_or_default()
}

fn write_spec(root: &std::path::Path, text: &str) -> Result<()> {
    fs::write(root.join("packaging/rpm/among.spec"), text)?;
    Ok(())
}

#[test]
fn real_spec_matches_the_packaged_payload_and_versions() -> Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or("project root is not resolvable")?;
    let report = rpm::check(root, &rpm::spec_path(root))?;
    assert_eq!(report["name"], "among");
    assert_eq!(report["version"], package_version::read(root)?);
    assert!(report["payload"]
        .as_array()
        .is_some_and(|paths| paths.contains(&serde_json::json!("%{_bindir}/among"))));
    Ok(())
}

#[test]
fn missing_field_wrong_version_and_unreferenced_payload_are_refused() -> Result<()> {
    let root = fixture()?;
    let original = spec_of(&root);

    // A field the spec must carry.
    write_spec(
        &root,
        &original.replace("Summary:        GNOME", "Note:           GNOME"),
    )?;
    assert!(rpm::check(&root, &rpm::spec_path(&root)).is_err());

    // A version that does not match what would actually be built.
    write_spec(
        &root,
        &original.replace("Version:        0.1.0", "Version:        0.9.9"),
    )?;
    assert!(rpm::check(&root, &rpm::spec_path(&root)).is_err());

    // A leading v, which is not a Fedora Version.
    write_spec(
        &root,
        &original.replace("Version:        0.1.0", "Version:        v0.1.0"),
    )?;
    assert!(rpm::check(&root, &rpm::spec_path(&root)).is_err());

    // A Release that is not distribution tagged.
    write_spec(
        &root,
        &original.replace("Release:        1%{?dist}", "Release:        1"),
    )?;
    assert!(rpm::check(&root, &rpm::spec_path(&root)).is_err());

    // Dropping the executable from %files would ship an empty package.
    write_spec(&root, &original.replace("%{_bindir}/among\n", ""))?;
    assert!(rpm::check(&root, &rpm::spec_path(&root)).is_err());

    // Removing a pkgconfig capability would silently understate the libraries.
    for capability in rpm::PKGCONFIG {
        let text = original
            .lines()
            .filter(|line| !line.contains(capability))
            .collect::<Vec<_>>()
            .join("\n");
        write_spec(&root, &format!("{text}\n"))?;
        assert!(
            rpm::check(&root, &rpm::spec_path(&root)).is_err(),
            "missing {capability} was accepted"
        );
    }

    write_spec(&root, &original)?;
    assert!(rpm::check(&root, &rpm::spec_path(&root)).is_ok());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_plugin_cannot_re_enter_the_package_unnoticed() -> Result<()> {
    let root = fixture()?;
    let original = spec_of(&root);
    let check = |root: &std::path::Path| rpm::check(root, &rpm::spec_path(root));
    // Official plugins are installed from their own repository: the spec may
    // neither build the removed Cargo package, nor install a plugin loop, nor
    // list or own anything below /usr/share/among.
    for (anchor, addition) in [
        (
            "cargo build --release --locked --manifest-path Cargo.toml\n",
            "cargo build --release --locked --manifest-path examples/plugins/Cargo.toml\n",
        ),
        (
            "cargo test --locked --manifest-path Cargo.toml\n",
            "cargo test --locked --manifest-path examples/plugins/Cargo.toml\n",
        ),
        (
            "mkdir -p %{buildroot}%{_datadir}/applications\n",
            "mkdir -p %{buildroot}%{_datadir}/among/plugins\n",
        ),
        (
            "install -m 0755 \"$CARGO_TARGET_DIR/release/among\" %{buildroot}%{_bindir}/among\n",
            "for plugin in clock system-status pane-pwd; do\n    true\ndone\n",
        ),
        ("%{_bindir}/among\n", "%{_datadir}/among/plugins\n"),
        (
            "%{_bindir}/among\n",
            "%{_prefix}/share/among/plugins/clock\n",
        ),
        ("%{_bindir}/among\n", "%dir %{_datadir}/among\n"),
    ] {
        assert!(original.contains(anchor), "{anchor:?}");
        write_spec(
            &root,
            &original.replacen(anchor, &format!("{anchor}{addition}"), 1),
        )?;
        assert!(check(&root).is_err(), "{addition:?} was accepted");
    }

    write_spec(&root, &original)?;
    assert!(rpm::check(&root, &rpm::spec_path(&root)).is_ok());

    // A comment that explains the removed directory installs nothing.
    let anchor = "%{_bindir}/among\n";
    write_spec(
        &root,
        &original.replacen(
            anchor,
            &format!("{anchor}# plugins used to live in /usr/share/among/plugins\n"),
            1,
        ),
    )?;
    assert!(rpm::check(&root, &rpm::spec_path(&root)).is_ok());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_spec_outside_the_project_or_named_with_a_version_is_refused() -> Result<()> {
    let root = fixture()?;
    let original = spec_of(&root);

    let elsewhere = storage::private_directory(&std::env::temp_dir(), "among-rpm-outside")?;
    fs::create_dir_all(elsewhere.join("packaging/rpm"))?;
    fs::write(elsewhere.join("packaging/rpm/among.spec"), &original)?;
    assert!(rpm::check(&root, &elsewhere.join("packaging/rpm/among.spec")).is_err());

    fs::rename(
        root.join("packaging/rpm/among.spec"),
        root.join("packaging/rpm/among-0.1.0.spec"),
    )?;
    assert!(rpm::check(&root, &root.join("packaging/rpm/among-0.1.0.spec")).is_err());

    fs::remove_dir_all(root)?;
    fs::remove_dir_all(elsewhere)?;
    Ok(())
}

#[test]
fn duplicated_headers_payload_paths_and_licenses_are_refused() -> Result<()> {
    let root = fixture()?;
    let original = spec_of(&root);
    let check = |root: &std::path::Path| rpm::check(root, &rpm::spec_path(root));

    // rpmbuild lets a later single-valued header override the first one, so the
    // value this checker validated would not be the value that gets built.
    for (line, duplicate) in [
        ("Version:        0.1.0\n", "Version:        0.2.0\n"),
        (
            "Name:           among\n",
            "Name:           among-terminal\n",
        ),
        ("Release:        1%{?dist}\n", "Release:        2%{?dist}\n"),
    ] {
        write_spec(
            &root,
            &original.replacen(line, &format!("{line}{duplicate}"), 1),
        )?;
        assert!(check(&root).is_err(), "second {duplicate:?} was accepted");
    }

    // The same packaged path twice: the second entry's attributes would win.
    write_spec(
        &root,
        &original.replace("%{_bindir}/among\n", "%{_bindir}/among\n%{_bindir}/among\n"),
    )?;
    assert!(check(&root).is_err());

    write_spec(
        &root,
        &original.replace("%license NOTICE\n", "%license NOTICE\n%license NOTICE\n"),
    )?;
    assert!(check(&root).is_err());

    write_spec(&root, &original)?;
    check(&root)?;
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn license_materials_must_exist_and_survive_nodocs_installs() -> Result<()> {
    let root = fixture()?;
    let original = spec_of(&root);
    let check = |root: &std::path::Path| rpm::check(root, &rpm::spec_path(root));

    // %doc files are skipped by tsflags=nodocs, the default in Fedora images.
    write_spec(
        &root,
        &original.replace("%license NOTICE\n", "%doc NOTICE\n"),
    )?;
    assert!(check(&root).is_err());
    write_spec(
        &root,
        &original.replace("%license THIRD_PARTY\n", "%doc THIRD_PARTY\n"),
    )?;
    assert!(check(&root).is_err());
    write_spec(&root, &original.replace("%license NOTICE\n", ""))?;
    assert!(check(&root).is_err());
    write_spec(&root, &original)?;

    // Declared but absent from the source tree: rpmbuild would fail late.
    for (path, restore) in [
        ("LICENSE", "synthetic license\n"),
        ("NOTICE", "synthetic notice\n"),
        ("THIRD_PARTY/inventory.json", "{}\n"),
    ] {
        fs::remove_file(root.join(path))?;
        assert!(check(&root).is_err(), "missing {path} was accepted");
        fs::write(root.join(path), restore)?;
    }
    check(&root)?;
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn the_spec_cannot_choose_or_restrict_the_package_architecture() -> Result<()> {
    let root = fixture()?;
    let original = spec_of(&root);
    // The native builder decides the architecture; a tag in the spec could
    // label an x86_64 executable as noarch or keep a target from building.
    for tag in [
        "BuildArch:      noarch",
        "BuildArch:      aarch64",
        "ExclusiveArch:  x86_64",
        "ExcludeArch:    aarch64",
    ] {
        write_spec(
            &root,
            &original.replacen(
                "Release:        1%{?dist}\n",
                &format!("Release:        1%{{?dist}}\n{tag}\n"),
                1,
            ),
        )?;
        assert!(
            rpm::check(&root, &rpm::spec_path(&root)).is_err(),
            "{tag} was accepted"
        );
    }
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn library_floors_follow_the_binding_api_level_and_cannot_be_understated() -> Result<()> {
    let root = fixture()?;
    let original = spec_of(&root);
    let check = |root: &std::path::Path| rpm::check(root, &rpm::spec_path(root));

    let minimums = rpm::binding_minimums(&root)?;
    assert_eq!(
        minimums,
        vec![
            ("pkgconfig(gtk4)".to_string(), (4, 12)),
            ("pkgconfig(libadwaita-1)".to_string(), (1, 5)),
            ("pkgconfig(vte-2.91-gtk4)".to_string(), (0, 76)),
        ]
    );

    // Each understated floor would admit a library missing symbols the
    // binding calls; a floor without a version admits any library at all.
    for (line, understated) in [
        ("pkgconfig(gtk4) >= 4.12", "pkgconfig(gtk4) >= 4.10"),
        (
            "pkgconfig(libadwaita-1) >= 1.5",
            "pkgconfig(libadwaita-1) >= 1.4",
        ),
        (
            "pkgconfig(vte-2.91-gtk4) >= 0.76",
            "pkgconfig(vte-2.91-gtk4) >= 0.70",
        ),
        ("pkgconfig(gtk4) >= 4.12", "pkgconfig(gtk4)"),
        ("pkgconfig(gtk4) >= 4.12", "pkgconfig(gtk4) > 4.12"),
    ] {
        write_spec(&root, &original.replace(line, understated))?;
        assert!(check(&root).is_err(), "{understated} was accepted");
    }

    // Raising the binding level without raising the floor is the drift this
    // check exists for: the spec would still claim GTK 4.12 is enough.
    write_spec(&root, &original)?;
    let manifest = fs::read_to_string(root.join("Cargo.toml"))?;
    fs::write(
        root.join("Cargo.toml"),
        manifest.replace(
            "features = [\"v4_10\", \"v4_12\"]",
            "features = [\"v4_10\", \"v4_14\"]",
        ),
    )?;
    assert!(check(&root).is_err());
    fs::write(root.join("Cargo.toml"), &manifest)?;

    let report = check(&root)?;
    assert_eq!(report["library_minimums"][0]["declared"], "4.12");
    assert_eq!(report["library_minimums"][0]["binding_api_level"], "4.12");
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn only_the_application_data_directory_is_forbidden_not_a_lookalike_sibling() -> Result<()> {
    for refused in [
        "%{_datadir}/among",
        "%{_prefix}/share/among/plugins/clock",
        "%{_datarootdir}/among/plugins",
        "%{_datadir}/among/plugins",
        "%dir %{_datadir}/among\n",
        "mkdir -p %{buildroot}%{_datadir}/among/plugins",
        "/usr/share/among/plugins/clock",
    ] {
        assert!(rpm::mentions_application_data(refused), "{refused:?}");
    }
    for allowed in [
        "%{_datadir}/among-terminal-data/file",
        "%{_datadir}/amongst",
        "/usr/share/among.d/x",
        "%{_datadir}/applications/com.github.among-terminal.desktop",
        "%{_datadir}/metainfo/com.github.among-terminal.metainfo.xml",
    ] {
        assert!(!rpm::mentions_application_data(allowed), "{allowed:?}");
    }
    Ok(())
}

#[test]
fn the_package_creates_no_application_data_directory() -> Result<()> {
    let spec = spec_of(&fixture()?);
    assert!(
        !spec.contains("_datadir}/among"),
        "a fresh package must create or own nothing under /usr/share/among"
    );
    Ok(())
}
