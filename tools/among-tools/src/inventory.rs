use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub path: String,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub sha256: String,
    pub planned_rust_operation: String,
    pub parity_status: String,
    // complete inert source preserves multiline assertions and embedded fixtures;
    // excerpts alone would lose conditions when Python statements span lines.
    pub baseline_source: String,
    pub imports_and_helpers: Vec<Reference>,
    pub scenarios_and_assertions: Vec<Reference>,
    pub outputs_and_prerequisites: Vec<Reference>,
    pub callers: Vec<Reference>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub schema: u32,
    pub sources: BTreeMap<String, Source>,
    pub direct_references: Vec<Reference>,
    // full caller text retains inline heredoc bodies, not just interpreter lines.
    pub caller_baselines: BTreeMap<String, String>,
}

fn files(directory: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(format!("owned input is a symlink: {}", path.display()).into());
        }
        if metadata.is_dir() {
            let name = path.file_name().and_then(|name| name.to_str());
            if matches!(
                name,
                Some("target" | "__pycache__" | "node_modules" | "out" | ".git")
            ) {
                continue;
            }
            files(&path, output)?;
        } else if metadata.is_file() {
            output.push(path);
        }
    }
    Ok(())
}

fn references(path: &str, text: &str, predicate: impl Fn(&str) -> bool) -> Vec<Reference> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| predicate(line))
        .map(|(line, text)| Reference {
            path: path.to_string(),
            line: line + 1,
            text: text.to_string(),
        })
        .collect()
}

pub fn discover(root: &Path) -> Result<Inventory> {
    let root = root.canonicalize()?;
    let mut paths = Vec::new();
    // private agent/evidence trees are not project callers. Video out is an
    // external/generated workshop, never an owned migration entry point.
    for directory in [
        "scripts",
        "video/readme-promo",
        "src",
        "docs",
        "packaging",
        "snap",
        ".github/workflows",
    ] {
        files(&root.join(directory), &mut paths)?;
    }
    for filename in ["README.md", "CONTRIBUTING.md"] {
        let path = root.join(filename);
        if path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();
    let mut content = BTreeMap::new();
    for path in paths {
        let relative = path.strip_prefix(&root)?.to_string_lossy().into_owned();
        match String::from_utf8(fs::read(path)?) {
            Ok(text) => {
                content.insert(relative, text);
            }
            Err(_) if relative.ends_with(".py") => {
                return Err(format!("owned Python source is not UTF-8: {relative}").into())
            }
            Err(_) => {}
        }
    }
    let mut sources = BTreeMap::new();
    let mut direct_references = Vec::new();
    let mut caller_baselines = BTreeMap::new();
    for (path, text) in &content {
        if !path.ends_with(".py") {
            if text.contains("python") || text.contains("Python") || text.contains(".py") {
                caller_baselines.insert(path.clone(), text.clone());
            }
            direct_references.extend(references(path, text, |line| {
                line.contains("python") || line.contains("Python") || line.contains(".py")
            }));
            continue;
        }
        let name = Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("invalid source filename")?;
        let stem = Path::new(path)
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or("invalid source stem")?;
        let module = stem.replace('-', "_");
        let mut callers = Vec::new();
        for (caller_path, caller_text) in &content {
            if caller_path != path {
                callers.extend(references(caller_path, caller_text, |line| {
                    line.contains(name)
                        || line.contains(&format!("\"{stem}\""))
                        || line.contains(&format!("'{stem}'"))
                        || line.contains(&module)
                }));
            }
        }
        let operation = if stem.starts_with("test-") {
            format!(
                "cargo test --locked --manifest-path tools/among-tools/Cargo.toml -- {}",
                module
            )
        } else {
            format!(
                "among-tools {}",
                if path.starts_with("video/") {
                    format!("video-{stem}")
                } else {
                    stem.to_string()
                }
            )
        };
        sources.insert(
            path.clone(),
            Source {
                sha256: crate::storage::sha256(text.as_bytes())?,
                planned_rust_operation: operation,
                parity_status: "pending: replacement and executed parity evidence required".into(),
                baseline_source: text.clone(),
                imports_and_helpers: references(path, text, |line| {
                    line.trim_start().starts_with("import ")
                        || line.trim_start().starts_with("from ")
                        || line.contains("load(")
                        || line.contains("spec_from_file_location")
                }),
                scenarios_and_assertions: references(path, text, |line| {
                    line.contains("assert")
                        || line.contains("raise ")
                        || line.contains("def test_")
                        || line.contains("PASS ")
                        || line.contains("check=True")
                }),
                outputs_and_prerequisites: references(path, text, |line| {
                    [
                        "write",
                        "mkdir",
                        "open(",
                        "subprocess",
                        "Popen",
                        "chmod",
                        "unlink",
                        "rename",
                        "replace",
                        "add_argument",
                        "ROOT",
                        "root =",
                    ]
                    .iter()
                    .any(|needle| line.contains(needle))
                }),
                callers,
            },
        );
    }
    if sources.is_empty() {
        return Err("no owned Python sources discovered; wrong project root".into());
    }
    Ok(Inventory {
        schema: 1,
        sources,
        direct_references,
        caller_baselines,
    })
}

pub fn freeze(path: &Path, inventory: &Inventory) -> Result<()> {
    let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
    output.write_all(&serde_json::to_vec_pretty(inventory)?)?;
    output.write_all(b"\n")?;
    output.sync_all()?;
    Ok(())
}

pub fn check(root: &Path, path: &Path) -> Result<()> {
    let expected: Inventory =
        serde_json::from_value(crate::storage::parse_json(&fs::read(path)?)?)?;
    if expected.schema != 1 || discover(root)? != expected {
        return Err("inventory drift: owned sources, assertions or callers changed; reconcile before cutover".into());
    }
    Ok(())
}
