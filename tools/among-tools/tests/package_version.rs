use among_tools::{package_version, Result};
use std::fs;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn actual_cli_parses_toml_and_reads_the_product_version_without_interpreter() -> Result<()> {
    let f = Fixture(among_tools::storage::private_directory(
        &std::env::temp_dir(),
        "among-version",
    )?);
    let main = f.0.join("Cargo.toml");
    for version in ["0.1.0", "1.2.3-rc.1"] {
        fs::write(
            &main,
            format!(
                "# version = 'not the package'\n[package]\nname='fixture'\nversion='{version}'\n"
            ),
        )?;
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .arg("package-version")
            .arg(&f.0)
            .output()?;
        assert!(result.status.success());
        assert_eq!(result.stdout, format!("{version}\n").as_bytes());
    }
    for invalid in [
        "[package]\nversion=1\n",
        "[package]\nversion=''\n",
        "[package]\nversion='0.1.0'\nversion='0.1.0'\n",
        "[package]\nversion='0.1\u{7}.0'\n",
        "[package]\nname='no-version'\n",
        "malformed",
    ] {
        fs::write(&main, invalid)?;
        assert!(package_version::read(&f.0).is_err(), "{invalid:?}");
    }
    // A stale plugin manifest next to the product manifest must not matter:
    // the plugins version independently in their own repository.
    fs::create_dir_all(f.0.join("examples/plugins"))?;
    fs::write(
        f.0.join("examples/plugins/Cargo.toml"),
        "[package]\nversion='9.9.9'\n",
    )?;
    fs::write(&main, "[package]\nversion='0.1.0'\n")?;
    assert_eq!(package_version::read(&f.0)?, "0.1.0");
    fs::remove_file(&main)?;
    let target = f.0.join("target.toml");
    fs::write(&target, "[package]\nversion='0.1.0'\n")?;
    std::os::unix::fs::symlink(&target, &main)?;
    assert!(package_version::read(&f.0).is_err());
    Ok(())
}
