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
fn actual_cli_parses_toml_and_checks_both_versions_without_interpreter() -> Result<()> {
    let f = Fixture(among_tools::storage::private_directory(
        &std::env::temp_dir(),
        "among-version",
    )?);
    fs::create_dir_all(f.0.join("examples/plugins"))?;
    let main = f.0.join("Cargo.toml");
    let plugins = f.0.join("examples/plugins/Cargo.toml");
    for version in ["0.1.0", "1.2.3-rc.1"] {
        for path in [&main, &plugins] {
            fs::write(path,format!("# version = 'not the package'\n[package]\nname='fixture'\nversion='{version}'\n"))?;
        }
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .arg("package-version")
            .arg(&f.0)
            .output()?;
        assert!(result.status.success());
        assert_eq!(result.stdout, format!("{version}\n").as_bytes());
    }
    for invalid in [
        "[package]\nversion='different'\n",
        "[package]\nversion=1\n",
        "[package]\nversion=''\n",
        "[package]\nversion='0.1.0'\nversion='0.1.0'\n",
        "malformed",
    ] {
        fs::write(&plugins, invalid)?;
        assert!(package_version::read(&f.0).is_err());
    }
    fs::remove_file(&plugins)?;
    std::os::unix::fs::symlink(&main, &plugins)?;
    assert!(package_version::read(&f.0).is_err());
    Ok(())
}
