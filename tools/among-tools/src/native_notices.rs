use crate::{storage, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};

fn payload(root: &Path, relative: &Path) -> Result<PathBuf> {
    storage::relative_path(relative)?;
    // Distribution documentation may link to another packaged license. Retain
    // those links only when their resolved target stays inside actual prime.
    let path = root.join(relative).canonicalize()?;
    if !path.starts_with(root) || !path.is_file() {
        return Err("native notice escapes payload or is not regular".into());
    }
    Ok(path)
}

pub fn check(document: &Value, prime: &Path) -> Result<Value> {
    let prime = prime.canonicalize()?;
    let packages = document["packages"]
        .as_array()
        .filter(|packages| !packages.is_empty())
        .ok_or("empty native notice inventory")?;
    if document["schema"] != 1 {
        return Err("unsupported native notice inventory".into());
    }
    let mut count = 0;
    let mut references = BTreeSet::new();
    for package in packages {
        let name = package["package"]
            .as_str()
            .ok_or("native package name absent")?;
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err("unsafe native package name".into());
        }
        let prefix = format!("notices/{name}/");
        for notice in package["notices"]
            .as_array()
            .ok_or("native notices absent")?
        {
            let path = notice["path"].as_str().ok_or("native notice path absent")?;
            let relative = path
                .strip_prefix(&prefix)
                .ok_or("unexpected native notice layout")?;
            let target = payload(&prime, Path::new(relative))?;
            if notice["sha256"] != storage::sha256(File::open(target)?)? {
                return Err("changed native notice text".into());
            }
            count += 1;
        }
        for name in package["common_license_references"]
            .as_array()
            .ok_or("native common license references absent")?
        {
            let name = name
                .as_str()
                .ok_or("common license reference must be a name")?;
            if name.is_empty()
                || Path::new(name).file_name().and_then(|name| name.to_str()) != Some(name)
            {
                return Err("unsafe common-license name".into());
            }
            references.insert(name);
        }
    }
    let mut licenses = Vec::new();
    for name in references {
        let target = payload(&prime, &Path::new("usr/share/common-licenses").join(name))?;
        if target.metadata()?.len() == 0 {
            return Err("empty referenced common-license text".into());
        }
        licenses.push(json!({"name":name,"sha256":storage::sha256(File::open(target)?)?}));
    }
    Ok(
        json!({"notices":count,"common_licenses":licenses,"scope":"verbatim native notices and referenced nonempty common-license files only","compliance":"not established; license compatibility/source/runtime assessment still required"}),
    )
}
