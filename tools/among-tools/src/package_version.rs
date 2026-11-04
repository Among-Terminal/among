use crate::{storage, Result};
use std::fs;
use std::path::Path;

/// The product version, read from the root package. The host owns exactly one
/// version; official plugins version independently in their own repository.
pub fn read(root: &Path) -> Result<String> {
    let document: toml::Value = toml::from_str(&fs::read_to_string(storage::safe_path(
        root,
        Path::new("Cargo.toml"),
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
    Ok(version.to_string())
}
