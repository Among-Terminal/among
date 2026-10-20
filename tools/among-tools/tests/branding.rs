use among_tools::{installation, release_metadata, storage, Result};
use std::{fs, path::Path};

#[test]
fn source_install_delivers_the_approved_vector_and_preserves_the_icon_identity() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let base = storage::private_directory(&std::env::temp_dir(), "among-branding")?;
    let layout =
        installation::Layout::new(&base.join("home"), &base.join("data"), &base.join("state"))?;
    let payload = installation::source_payload(
        &root,
        Path::new(env!("CARGO_BIN_EXE_among-tools")),
        Path::new(env!("CARGO_BIN_EXE_among-uninstall")),
        &layout,
    )?;
    let expected = include_bytes!("../../../docs/media/logo-small.svg");
    assert_eq!(payload["icon"].bytes, expected);
    assert!(String::from_utf8_lossy(&payload["desktop"].bytes)
        .contains("Icon=com.github.among-terminal\n"));
    let icon = layout.destinations["icon"].clone();
    installation::install_payload(layout, payload)?;
    assert_eq!(fs::read(icon)?, expected);
    installation::uninstall(installation::Layout::new(
        &base.join("home"),
        &base.join("data"),
        &base.join("state"),
    )?)?;
    fs::remove_dir_all(base)?;
    Ok(())
}

#[test]
fn approved_assets_keep_their_square_canvas_and_raster_dimensions() -> Result<()> {
    let vector = include_str!("../../../docs/media/logo-small.svg");
    let document = roxmltree::Document::parse(vector)?;
    let svg = document.root_element();
    assert_eq!(svg.attribute("viewBox"), Some("0 0 559 559"));
    assert_eq!(svg.attribute("width"), svg.attribute("height"));
    assert!(!document
        .descendants()
        .any(|node| node.has_tag_name("image")));
    let png = include_bytes!("../../../docs/media/logo-small.png");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&png[12..16], b"IHDR");
    assert_eq!(&png[16..20], &559u32.to_be_bytes());
    assert_eq!(&png[20..24], &559u32.to_be_bytes());
    Ok(())
}

#[test]
fn rpm_and_snap_select_and_own_the_same_packaged_vector() -> Result<()> {
    let rpm = include_str!("../../../packaging/rpm/among.spec").replace("\\\n", " ");
    assert!(rpm
        .lines()
        .any(|line| line.split_whitespace().collect::<Vec<_>>()
            == [
                "install",
                "-m",
                "0644",
                "data/com.github.among-terminal.svg",
                "%{buildroot}%{_datadir}/icons/hicolor/scalable/apps/",
            ]));
    let files = rpm
        .split_once("%files\n")
        .ok_or("RPM ownership section missing")?
        .1;
    assert!(files.lines().any(|line| line.trim()
        == "%{_datadir}/icons/hicolor/scalable/apps/com.github.among-terminal.svg"));
    let snap = among_tools::deb_workflow::read(include_str!("../../../snap/snapcraft.yaml"))?;
    let build = snap["parts"]["among"]["override-build"]
        .as_str()
        .ok_or("Snap build script missing")?;
    assert!(build.lines().any(|line| line.split_whitespace().collect::<Vec<_>>() == [
        "install", "-Dm644", "data/com.github.among-terminal.svg",
        "\"$CRAFT_PART_INSTALL/usr/share/icons/hicolor/scalable/apps/com.github.among-terminal.svg\"",
    ]));
    assert!(build.contains("s|^Icon=com.github.among-terminal$|Icon=usr/share/icons/hicolor/scalable/apps/com.github.among-terminal.svg|"));
    Ok(())
}

#[test]
fn artwork_gate_rejects_each_missing_or_changed_input() -> Result<()> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-artwork-gate")?;
    fs::create_dir_all(root.join("docs/media"))?;
    fs::create_dir(root.join("data"))?;
    let assets: [(&str, &[u8]); 5] = [
        (
            "data/com.github.among-terminal-128.png",
            include_bytes!("../../../data/com.github.among-terminal-128.png"),
        ),
        (
            "data/com.github.among-terminal-256.png",
            include_bytes!("../../../data/com.github.among-terminal-256.png"),
        ),
        (
            "docs/media/logo-small.svg",
            include_bytes!("../../../docs/media/logo-small.svg"),
        ),
        (
            "docs/media/logo-small.png",
            include_bytes!("../../../docs/media/logo-small.png"),
        ),
        (
            "data/com.github.among-terminal.svg",
            include_bytes!("../../../docs/media/logo-small.svg"),
        ),
    ];
    for (path, bytes) in assets {
        fs::write(root.join(path), bytes)?;
    }
    release_metadata::artwork(&root)?;
    for (path, bytes) in assets {
        fs::remove_file(root.join(path))?;
        assert!(release_metadata::artwork(&root).is_err(), "missing {path}");
        fs::write(root.join(path), b"wrong artwork")?;
        assert!(release_metadata::artwork(&root).is_err(), "changed {path}");
        fs::write(root.join(path), bytes)?;
    }
    release_metadata::artwork(&root)?;
    fs::remove_dir_all(root)?;
    Ok(())
}
