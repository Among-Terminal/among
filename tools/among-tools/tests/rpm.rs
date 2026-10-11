use among_tools::{rpm, storage, Result};
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
    fs::create_dir_all(root.join("examples/plugins"))?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"among\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    fs::write(
        root.join("examples/plugins/Cargo.toml"),
        "[package]\nname = \"plugins\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    fs::write(
        root.join("data/com.github.among-terminal.desktop"),
        "[Desktop Entry]\nIcon=com.github.among-terminal\n",
    )?;
    fs::write(
        root.join("data/com.github.among-terminal.metainfo.xml"),
        "<component><id>com.github.among-terminal</id></component>\n",
    )?;
    fs::write(
        root.join("packaging/rpm/among.spec"),
        include_str!("../../../packaging/rpm/among.spec"),
    )?;
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
    assert_eq!(report["version"], "0.1.0");
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
fn a_new_example_plugin_cannot_enter_the_package_unnoticed() -> Result<()> {
    let root = fixture()?;
    let original = spec_of(&root);
    // Adding an example to the install loop without extending the recorded set
    // would ship a plugin the Snap and Debian packages do not contain.
    write_spec(
        &root,
        &original.replace(
            "for plugin in clock system-status pane-pwd; do",
            "for plugin in clock system-status pane-pwd git-state; do",
        ),
    )?;
    assert!(rpm::check(&root, &rpm::spec_path(&root)).is_err());

    // Dropping a shipped example must be refused just as firmly.
    write_spec(
        &root,
        &original.replace(
            "for plugin in clock system-status pane-pwd; do",
            "for plugin in clock system-status; do",
        ),
    )?;
    assert!(rpm::check(&root, &rpm::spec_path(&root)).is_err());

    write_spec(&root, &original)?;
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
