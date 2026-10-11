use among_tools::{python_guard, storage, Result};
use std::{fs, path::PathBuf};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn initial_guard_reports_owned_sources_shebangs_commands_dependencies_and_multiline_delegation() {
    for (path, text, line) in [
        ("scripts/owned.py", "print(1)", 1),
        ("scripts/extensionless", "#!/usr/bin/env python3\n", 1),
        (
            ".github/workflows/check.yml",
            "run: |\n  python3 - <<'EOF'\n",
            2,
        ),
        (
            "package.json",
            "{\"scripts\":{\"test\":\"python3 scripts/test.py\"}}",
            1,
        ),
        ("snap/snapcraft.yaml", "build-packages: [python3-gi]\n", 1),
        ("README.md", "```sh\npython3 scripts/run.py\n```\n", 2),
        ("src/helper.rs", "Command :: new(\n  \"python3\"\n)", 1),
    ] {
        let findings = python_guard::inspect(path, text);
        assert!(
            findings.iter().any(|finding| finding.line == line),
            "expected path/line finding for {path}"
        );
    }
    assert!(python_guard::inspect(
        "README.md",
        "Python is not a supported project interpreter.\n"
    )
    .is_empty());
    assert!(python_guard::inspect(
        "src/helper.rs",
        "// Command::new(\"python3\") is forbidden\n"
    )
    .is_empty());
}
#[test]
fn rust_ast_scans_actual_raw_and_argument_calls_but_keeps_literals_and_comments_inert() {
    for text in [
        "Command::new(r#\"/usr/bin/python3\"#)",
        "Command::new(\"env\").args([\"python3\", \"script\"])",
        "Command::new(\"sh\").arg(\"-c\").arg(r#\"python3 scripts/runner.py\"#)",
        "Command::new(concat!(\"py\", \"thon3\"))",
    ] {
        assert!(
            python_guard::inspect("src/run.rs", text)
                .iter()
                .any(|finding| finding.reason == "Rust interpreter delegation"),
            "AST missed actual delegation: {text}"
        );
    }
    for text in [
        "const INERT: &str = r#\"Command::new(\\\"python3\\\")\"#;",
        "/* Command::new(\"python3\") */ fn main() {}",
        "fn main() { std::process::Command::new(\"among-tools\").arg(\"check-no-python\"); }",
    ] {
        assert!(
            python_guard::inspect("src/run.rs", text).is_empty(),
            "AST treated inert literal/comment as runtime delegation"
        );
    }
    assert!(python_guard::inspect("src/broken.rs", "fn broken(")
        .iter()
        .any(|finding| finding.reason.contains("unparseable Rust")));
}
#[test]
fn direct_dependency_manifests_bindings_and_binary_requirements_cannot_hide_in_noncommand_files(
) -> Result<()> {
    for name in [
        "requirements.txt",
        "requirements-dev.txt",
        "pyproject.toml",
        "Pipfile",
        "Pipfile.lock",
        "uv.lock",
        "setup.cfg",
    ] {
        assert!(python_guard::inspect(name, "inert-looking fixture")
            .iter()
            .any(|finding| finding.reason == "owned Python dependency manifest"));
    }
    for (path, text) in [
        ("Dockerfile", "RUN apt-get install python3-dev"),
        ("Cargo.toml", "pyo3 = \"0.1\""),
        ("Cargo.toml", "[dependencies.pyo3]"),
        (
            "package.json",
            "{\"dependencies\":{\"PyGObject\":\"fixture\"}}",
        ),
        ("packaging/debian/control", "Build-Depends: python3-minimal"),
    ] {
        assert!(python_guard::inspect(path, text)
            .iter()
            .any(|finding| finding.reason == "explicit Python tooling dependency"));
    }
    let f = Fixture(storage::private_directory(
        &std::env::temp_dir(),
        "among-python-manifest",
    )?);
    fs::write(f.0.join("requirements.txt"), [0xff, 0])?;
    assert!(python_guard::scan(&f.0)?
        .iter()
        .any(|finding| finding.reason == "owned Python dependency manifest"));
    Ok(())
}
#[test]
fn owned_node_callers_are_not_excluded_with_the_external_rendering_engine() {
    for path in ["video/readme-promo/tools/helper.mjs", "scripts/tool.ts"] {
        assert!(
            python_guard::inspect(path, "spawn('python3', ['scripts/run.py']);")
                .iter()
                .any(|finding| finding.reason.contains("interpreter command"))
        );
        assert!(python_guard::inspect(
            path,
            "// Python examples in comments are inert\nspawn('ffmpeg', ['-version']);"
        )
        .is_empty());
    }
}

#[test]
fn dynamic_rust_execution_cannot_silently_pass_and_transparent_wrappers_are_scanned() {
    for text in [
        "fn run(program: &str) { Command::new(program); }",
        "fn run() { let program = concat!(\"py\", \"thon3\"); Command::new(program); }",
        "fn run() { Command::new(format!(\"{}{}\", \"py\", \"thon3\")); }",
        "fn run() { Command::new(std::env::var(\"RUNTIME\").unwrap()); }",
    ] {
        assert!(
            python_guard::inspect("src/run.rs", text)
                .iter()
                .any(|finding| finding
                    .reason
                    .contains("unresolved Rust dynamic executable")),
            "uncertified dynamic call passed: {text}"
        );
    }
    for text in [
        "fn run() { Command::new(Path::new(\"python3\")); }",
        "fn run() { Command::new(std::ffi::OsString::from(\"python3\")); }",
        "fn run() { Command::new(\"python3\".to_owned()); }",
        "fn run() { Command::new(\"py\".to_string() + \"thon3\"); }",
    ] {
        assert!(
            python_guard::inspect("src/run.rs", text)
                .iter()
                .any(|finding| finding.reason == "Rust interpreter delegation"),
            "wrapped interpreter call passed: {text}"
        );
    }
    for text in [
        "fn run() { Command::new(Path::new(\"/usr/bin/ffmpeg\")); }",
        "fn run() { Command::new(\"ffmpeg\".to_owned()); }",
        "const EXAMPLE: &str = \"Command::new(program)\";",
        "// Command::new(program)\nfn run() {}",
    ] {
        assert!(
            python_guard::inspect("src/run.rs", text).is_empty(),
            "inert/known native call was rejected: {text}"
        );
    }
}
#[test]
fn rust_calls_into_excluded_python_caches_remain_active_prohibition_findings() {
    for text in [
        "Command::new(\"vendor/example/tool.py\")",
        "Command::new(\"helper\").args([\"video/readme-promo/out/workbench-cache/render.py\"])",
    ] {
        assert!(python_guard::inspect("src/run.rs", text)
            .iter()
            .any(|finding| finding.reason.contains("Rust Python caller reference")));
    }
}
#[test]
fn unreadable_text_and_large_inputs_refuse_certification_without_loading_media_whole() -> Result<()>
{
    let f = Fixture(storage::private_directory(
        &std::env::temp_dir(),
        "among-python-bounds",
    )?);
    fs::write(
        f.0.join("run.sh"),
        b"#!/bin/sh\n\xff python3 scripts/run.py\n",
    )?;
    fs::write(f.0.join("native-media.mp4"), [0xff, 0xfe, 0])?;
    fs::File::create(f.0.join("large.md"))?.set_len(16 * 1024 * 1024 + 1)?;
    let findings = python_guard::scan(&f.0)?;
    assert!(findings
        .iter()
        .any(|finding| finding.path == "run.sh"
            && finding.reason.contains("non-UTF-8 owned automation")));
    assert!(findings
        .iter()
        .any(|finding| finding.path == "large.md" && finding.reason.contains("oversized text")));
    assert!(!findings
        .iter()
        .any(|finding| finding.path == "native-media.mp4"));
    Ok(())
}
#[test]
fn actual_cli_fails_with_path_locations_and_preserves_owned_files_without_executing_them(
) -> Result<()> {
    let f = Fixture(storage::private_directory(
        &std::env::temp_dir(),
        "among-python-guard",
    )?);
    fs::create_dir(f.0.join("scripts"))?;
    let file = f.0.join("scripts/owned.py");
    fs::write(
        &file,
        b"raise RuntimeError('this inert file must never execute')\n",
    )?;
    let before = fs::read(&file)?;
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("check-no-python")
        .arg(&f.0)
        .output()?;
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8(result.stderr)?.contains("scripts/owned.py:1: owned Python source"));
    assert_eq!(fs::read(file)?, before);
    let extensionless = f.0.join("scripts/binary-body");
    fs::write(extensionless, b"#!/usr/bin/env python3\n\xff\0\n")?;
    assert!(python_guard::scan(&f.0)?
        .iter()
        .any(|finding| finding.path == "scripts/binary-body"
            && finding.reason == "Python executable shebang"));
    Ok(())
}
#[test]
fn bounded_external_and_frozen_historical_inputs_are_inert_but_active_refs_and_symlinks_fail(
) -> Result<()> {
    let f = Fixture(storage::private_directory(
        &std::env::temp_dir(),
        "among-python-exclusion",
    )?);
    for name in [
        "vendor/upstream/example.py",
        "video/readme-promo/out/workbench-cache/example.py",
        "openspec/changes/archive/2020/history.py",
    ] {
        let path = f.0.join(name);
        fs::create_dir_all(path.parent().ok_or("parent absent")?)?;
        fs::write(path, b"inert external/historical content")?;
    }
    assert!(python_guard::scan(&f.0)?.is_empty());
    fs::write(
        f.0.join("README.md"),
        b"```sh\npython3 video/readme-promo/out/workbench-cache/example.py\n```\n",
    )?;
    assert!(!python_guard::scan(&f.0)?.is_empty());
    fs::remove_file(f.0.join("README.md"))?;
    std::os::unix::fs::symlink("vendor/upstream", f.0.join("scripts"))?;
    assert!(python_guard::scan(&f.0)?
        .iter()
        .any(|finding| finding.reason.contains("symlink")));
    Ok(())
}
