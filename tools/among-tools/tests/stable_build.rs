use among_tools::{source_snapshot, stable_build, storage, Result};
use std::fs;
use std::path::PathBuf;

const RECIPE: &str =
    "# keep comment\nname: among\nbase: core24\nconfinement: classic\ngrade: devel\n";
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
impl Fixture {
    fn new() -> Result<Self> {
        let base = storage::private_directory(&std::env::temp_dir(), "among-stable")?;
        let f = Self(base);
        fs::create_dir(f.root())?;
        for name in source_snapshot::FILES {
            let path = f.root().join(name);
            fs::create_dir_all(path.parent().ok_or("fixture parent absent")?)?;
            fs::write(path, b"fixture\n")?;
        }
        for name in ["Cargo.toml", "examples/plugins/Cargo.toml"] {
            fs::write(f.root().join(name), "[package]\nversion=\"0.1.0\"\n")?;
        }
        fs::create_dir(f.root().join("snap"))?;
        fs::write(f.root().join("snap/snapcraft.yaml"), RECIPE)?;
        fs::write(
            f.root().join(".secret"),
            b"synthetic private exclusion marker",
        )?;
        fs::create_dir(f.root().join("target"))?;
        fs::write(f.root().join("target/cache"), b"synthetic cache")?;
        Ok(f)
    }
    fn root(&self) -> PathBuf {
        self.0.join("repo")
    }
    fn output(&self) -> PathBuf {
        self.0.join("build")
    }
    fn prepare(&self) -> Result<serde_json::Value> {
        stable_build::prepare(&self.root(), &self.output(), "v0.1.0")
    }
}

#[test]
fn actual_cli_changes_only_grade_preserves_original_source_and_rejects_changed_output() -> Result<()>
{
    let f = Fixture::new()?;
    let example = f.root().join("examples/plugins/examples/install.rs");
    fs::create_dir_all(example.parent().ok_or("example parent absent")?)?;
    fs::write(&example, b"fn main() {}\n")?;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("prepare-stable-build")
        .arg(f.root())
        .arg(f.output())
        .arg("v0.1.0")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let record = stable_build::check(&f.output())?;
    assert_eq!(record["version"], "0.1.0");
    assert!(record["original_inputs"]
        .get("examples/plugins/examples/install.rs")
        .is_some());
    assert_eq!(
        fs::read_to_string(f.root().join("snap/snapcraft.yaml"))?,
        RECIPE
    );
    assert_eq!(
        fs::read_to_string(f.output().join("snap/snapcraft.devel.yaml"))?,
        RECIPE
    );
    assert_eq!(
        fs::read_to_string(f.output().join("snap/snapcraft.yaml"))?,
        RECIPE.replace("grade: devel", "grade: stable")
    );
    assert_eq!(
        fs::read(f.output().join("examples/plugins/examples/install.rs"))?,
        fs::read(example)?
    );
    assert!(!f.output().join(".secret").exists());
    assert!(!f.output().join("target").exists());
    let run = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .arg("check-stable-build")
            .arg(f.output())
            .output()
    };
    assert!(run()?.status.success());
    fs::write(f.output().join("snap/snapcraft.yaml"), RECIPE)?;
    assert!(!run()?.status.success());
    fs::write(
        f.output().join("snap/snapcraft.yaml"),
        RECIPE.replace("grade: devel", "grade: stable"),
    )?;
    fs::write(f.output().join("LICENSE"), b"changed")?;
    assert!(stable_build::check(&f.output()).is_err());
    Ok(())
}

#[test]
fn invalid_tags_versions_destinations_and_source_links_preserve_owner_state() -> Result<()> {
    let f = Fixture::new()?;
    for tag in ["v01.1.0", "v0.1.0-rc1", "v0.2.0"] {
        assert!(stable_build::prepare(&f.root(), &f.output(), tag).is_err());
        assert!(!f.output().exists());
    }
    fs::write(
        f.root().join("examples/plugins/Cargo.toml"),
        "[package]\nversion=\"0.2.0\"\n",
    )?;
    assert!(f.prepare().is_err());
    fs::write(
        f.root().join("examples/plugins/Cargo.toml"),
        "[package]\nversion=\"0.1.0\"\n",
    )?;
    assert!(stable_build::prepare(&f.root(), &f.root().join("build"), "v0.1.0").is_err());
    fs::remove_file(f.root().join("NOTICE"))?;
    std::os::unix::fs::symlink(f.root().join(".secret"), f.root().join("NOTICE"))?;
    assert!(f.prepare().is_err());
    assert!(!f.output().exists());
    fs::remove_file(f.root().join("NOTICE"))?;
    fs::write(f.root().join("NOTICE"), b"fixture")?;
    fs::create_dir(f.output())?;
    fs::write(f.output().join("owner"), b"keep")?;
    assert!(f.prepare().is_err());
    assert_eq!(fs::read(f.output().join("owner"))?, b"keep");
    let link = f.0.join("link");
    std::os::unix::fs::symlink(f.output(), &link)?;
    assert!(stable_build::prepare(&f.root(), &link, "v0.1.0").is_err());
    assert_eq!(fs::read(f.output().join("owner"))?, b"keep");
    Ok(())
}

#[test]
fn ambiguous_grade_extra_mutation_existing_overlay_and_unsafe_record_are_refused() -> Result<()> {
    for recipe in [
        format!("{RECIPE}grade: devel\n"),
        RECIPE.replace("grade: devel", "grade: 'devel'"),
        RECIPE.replace("grade: devel", "grade: &candidate devel"),
        RECIPE.replace("classic", "strict"),
    ] {
        let f = Fixture::new()?;
        fs::write(f.root().join("snap/snapcraft.yaml"), recipe)?;
        assert!(f.prepare().is_err());
        assert!(!f.output().exists());
    }
    assert!(stable_build::recipe_change(
        RECIPE,
        &RECIPE
            .replace("grade: devel", "grade: stable")
            .replace("core24", "core26")
    )
    .is_err());
    let f = Fixture::new()?;
    fs::write(
        f.root().join("snap/stable-build-input.json"),
        b"existing overlay",
    )?;
    assert!(f.prepare().is_err());
    assert!(!f.output().exists());
    let f = Fixture::new()?;
    let mut record = f.prepare()?;
    record["original_inputs"]["../foreign"] = serde_json::json!("a".repeat(64));
    fs::write(
        f.output().join("snap/stable-build-input.json"),
        serde_json::to_vec(&record)?,
    )?;
    assert!(stable_build::check(&f.output()).is_err());
    Ok(())
}
