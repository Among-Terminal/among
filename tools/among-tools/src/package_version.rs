use crate::{storage, Result};
use std::fs;
use std::path::Path;

pub fn read(root: &Path) -> Result<String> {
    let mut expected = None;
    for manifest in ["Cargo.toml", "examples/plugins/Cargo.toml"] {
        let document: toml::Value = toml::from_str(&fs::read_to_string(storage::safe_path(
            root,
            Path::new(manifest),
        )?)?)?;
        let version = document
            .get("package")
            .and_then(|package| package.get("version"))
            .and_then(toml::Value::as_str)
            .filter(|version| !version.is_empty())
            .ok_or("package version must be a nonempty TOML string")?;
        if version.chars().any(char::is_control) {
            return Err("package version contains a control character".into());
        }
        if expected
            .as_ref()
            .is_some_and(|expected| expected != version)
        {
            return Err("product/plugin versions differ".into());
        }
        expected = Some(version.to_string());
    }
    expected.ok_or_else(|| "package version absent".into())
}
