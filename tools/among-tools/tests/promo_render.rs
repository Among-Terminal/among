use among_tools::{promo_render, storage, Result};
use std::{fs, path::Path};

#[test]
fn rendering_retains_original_codec_silence_priority_resource_and_composition_arguments(
) -> Result<()> {
    let args = promo_render::render_arguments(
        Path::new("/private/src/index.ts"),
        Path::new("/private/public"),
        Path::new("/private/movie.mp4"),
    )?;
    assert_eq!(
        &args[..4],
        [
            "render",
            "/private/src/index.ts",
            "AmongPromo",
            "/private/movie.mp4"
        ]
    );
    for flag in [
        "--codec=h264",
        "--crf=20",
        "--concurrency=1",
        "--muted",
        "--timeout=180000",
        "--offthreadvideo-cache-size-in-bytes=134217728",
        "--log=error",
        "--public-dir=/private/public",
    ] {
        assert!(args.iter().any(|arg| arg == flag));
    }
    assert!(!args.iter().any(|arg| arg.contains("workbench-cache")));
    Ok(())
}

#[test]
fn actual_render_cli_refuses_missing_dependencies_without_creating_output() -> Result<()> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-promo-render")?;
    let output = root.join("must-not-exist");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("render-readme-promo")
        .arg(&root)
        .arg(&output)
        .output()?;
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(!output.exists());
    let action = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("render-readme-action")
        .arg(&root)
        .arg(&output)
        .output()?;
    assert!(!action.status.success());
    assert!(action.stdout.is_empty());
    assert!(!output.exists());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn npm_render_caller_targets_native_command_and_requires_explicit_private_output() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("project root absent")?;
    // The README video tooling is release tooling kept outside this repository,
    // the same way the capture harness is. When it is absent the wiring it
    // describes cannot be checked, and that is reported rather than passed
    // silently.
    let manifest = root.join("video/readme-promo/package.json");
    if !manifest.is_file() {
        eprintln!(
            "SKIPPED: video/readme-promo/package.json is absent; the README video render \
             wiring was not checked."
        );
        return Ok(());
    }
    let package = storage::parse_json(&fs::read(&manifest)?)?;
    let render = package["scripts"]["render"]
        .as_str()
        .ok_or("render script absent")?;
    assert_eq!(
        render,
        "../../../tools/among-tools/target/release/among-tools render-readme-promo ../../.."
    );
    // npm runs the script from the package's real directory, behind the
    // `video` link, so the relative root must climb out of that directory and
    // land on a checkout, not merely look right as a string.
    let package_directory = fs::canonicalize(manifest.parent().ok_or("package directory absent")?)?;
    let root_argument = render.rsplit(' ').next().ok_or("root argument absent")?;
    assert!(package_directory
        .join(root_argument)
        .join("tools/among-tools/Cargo.toml")
        .is_file());
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("render-readme-promo")
        .arg(root)
        .output()?;
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8(result.stderr)?.contains("invalid command"));
    Ok(())
}
