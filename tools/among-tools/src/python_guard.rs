use crate::Result;
use serde::Serialize;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub path: String,
    pub line: usize,
    pub reason: String,
}

fn external(path: &str) -> bool {
    [
        ".git",
        ".opencode",
        "vendor",
        "target",
        "tools/among-tools/target",
        "examples/plugins/target",
        "node_modules",
        "video/readme-promo/node_modules",
        "video/readme-promo/out/workbench-cache",
    ]
    .iter()
    .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
}
fn historical(path: &str) -> bool {
    path.starts_with("openspec/changes/archive/")
        || path == "openspec/changes/migrate-python-tooling-to-rust/migration-inventory.json"
}
fn dependency_manifest(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            matches!(
                name,
                "pyproject.toml"
                    | "Pipfile"
                    | "Pipfile.lock"
                    | "poetry.lock"
                    | "uv.lock"
                    | "setup.cfg"
            ) || (name.starts_with("requirements") && name.ends_with(".txt"))
        })
}
fn dependency(word: &str) -> bool {
    let normalized = word.rsplit('.').next().unwrap_or(word).to_ascii_lowercase();
    normalized.starts_with("python3-")
        || matches!(
            normalized.as_str(),
            "python"
                | "python3"
                | "pygobject"
                | "pyyaml"
                | "pycairo"
                | "pyo3"
                | "rustpython"
                | "rustpython-vm"
        )
}
pub(super) fn interpreter(word: &str) -> bool {
    let name = word.rsplit('/').next().unwrap_or(word);
    matches!(name, "python" | "pypy" | "pypy3")
        || name.strip_prefix("python").is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'.')
        })
}
pub(super) fn tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|character: char| {
        character.is_whitespace() || "\"'`;,|&()[]{}=<>".contains(character)
    })
    .filter(|word| !word.is_empty())
}
pub(super) fn add(findings: &mut Vec<Finding>, path: &str, line: usize, reason: &str) {
    findings.push(Finding {
        path: path.into(),
        line,
        reason: reason.into(),
    });
}

fn textual_input(path: &str) -> bool {
    let path = Path::new(path);
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some(
            "py" | "rs"
                | "sh"
                | "bash"
                | "yml"
                | "yaml"
                | "json"
                | "toml"
                | "md"
                | "mk"
                | "make"
                | "js"
                | "mjs"
                | "cjs"
                | "ts"
                | "tsx"
                | "jsx"
        )
    ) || path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "Makefile" | "Dockerfile" | "rules" | "control"))
}

pub fn inspect(path: &str, text: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    if path.ends_with(".py") {
        add(&mut findings, path, 1, "owned Python source");
    }
    if dependency_manifest(path) {
        add(&mut findings, path, 1, "owned Python dependency manifest");
    }
    if text.lines().next().is_some_and(|line| {
        line.starts_with("#!") && tokens(line.trim_start_matches("#!")).any(interpreter)
    }) {
        add(&mut findings, path, 1, "Python executable shebang");
    }
    let extension = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    let rust = extension == "rs";
    let active = textual_input(path) || text.starts_with("#!");
    if rust {
        crate::python_guard_rust::inspect(path, text, &mut findings);
    } else if active {
        let mut fenced = false;
        for (index, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with("```") {
                fenced = !fenced;
                continue;
            }
            if trimmed.starts_with('#') || trimmed.starts_with("//") {
                continue;
            }
            let command_context = extension != "md" || fenced || trimmed.contains('`');
            if command_context && tokens(trimmed).any(interpreter) {
                add(
                    &mut findings,
                    path,
                    index + 1,
                    "direct Python interpreter command or build input",
                );
            }
            if command_context && tokens(trimmed).any(dependency) {
                add(
                    &mut findings,
                    path,
                    index + 1,
                    "explicit Python tooling dependency",
                );
            }
            if command_context
                && tokens(trimmed)
                    .any(|token| token.ends_with(".py") && !matches!(token, ".py" | "*.py"))
            {
                add(
                    &mut findings,
                    path,
                    index + 1,
                    "owned Python caller reference",
                );
            }
        }
    }
    findings.sort_by(|left, right| {
        (&left.path, left.line, &left.reason).cmp(&(&right.path, right.line, &right.reason))
    });
    findings.dedup();
    findings
}

fn visit(root: &Path, relative: &Path, findings: &mut Vec<Finding>) -> Result<()> {
    let path = root.join(relative);
    let name = relative.to_str().ok_or("owned guard path is not UTF-8")?;
    if external(name) || historical(name) {
        return Ok(());
    }
    let kind = path.symlink_metadata()?.file_type();
    if kind.is_symlink() {
        add(
            findings,
            name,
            1,
            "owned input is a symlink; cannot certify inert exclusion",
        );
        return Ok(());
    }
    if kind.is_dir() {
        let mut entries = fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort();
        for entry in entries {
            visit(root, &relative.join(entry), findings)?;
        }
    } else if kind.is_file() {
        let mut input = fs::File::open(&path)?;
        let size = input.metadata()?.len();
        let mut prefix = vec![0; usize::try_from(size.min(4096))?];
        input.read_exact(&mut prefix)?;
        if let Some(first) = prefix
            .split(|byte| *byte == b'\n')
            .next()
            .and_then(|line| std::str::from_utf8(line).ok())
        {
            if first.starts_with("#!") && tokens(first.trim_start_matches("#!")).any(interpreter) {
                add(findings, name, 1, "Python executable shebang");
            }
        }
        if name.ends_with(".py") {
            add(findings, name, 1, "owned Python source");
        }
        if dependency_manifest(name) {
            add(findings, name, 1, "owned Python dependency manifest");
        }
        if std::str::from_utf8(&prefix).is_err() {
            if name.ends_with(".py") {
                add(findings, name, 1, "non-UTF-8 owned Python source");
            }
            if textual_input(name) || prefix.starts_with(b"#!") {
                add(
                    findings,
                    name,
                    1,
                    "non-UTF-8 owned automation; cannot certify prohibition scan",
                );
            }
            return Ok(());
        }
        // Raw video, ELF and archives must not be loaded whole into memory.
        // Oversized textual inputs fail rather than silently escaping a scan.
        if size > 16 * 1024 * 1024 {
            add(
                findings,
                name,
                1,
                "oversized text input; cannot certify complete prohibition scan",
            );
            return Ok(());
        }
        let mut bytes = prefix;
        input.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 16 * 1024 * 1024 {
            add(
                findings,
                name,
                1,
                "text input grew beyond scan limit; cannot certify prohibition scan",
            );
            return Ok(());
        }
        match std::str::from_utf8(&bytes) {
            Ok(text) => findings.extend(inspect(name, text)),
            Err(_) if name.ends_with(".py") => {
                add(findings, name, 1, "non-UTF-8 owned Python source")
            }
            Err(_) if textual_input(name) || bytes.starts_with(b"#!") => add(
                findings,
                name,
                1,
                "non-UTF-8 owned automation; cannot certify prohibition scan",
            ),
            Err(_) => {}
        }
    } else {
        add(findings, name, 1, "unsupported owned filesystem entry");
    }
    Ok(())
}

pub fn scan(root: &Path) -> Result<Vec<Finding>> {
    if root.symlink_metadata()?.file_type().is_symlink() || !root.is_dir() {
        return Err("Python guard requires a real source directory".into());
    }
    let mut findings = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        visit(root, &PathBuf::from(entry.file_name()), &mut findings)?;
    }
    findings.sort_by(|left, right| {
        (&left.path, left.line, &left.reason).cmp(&(&right.path, right.line, &right.reason))
    });
    findings.dedup();
    Ok(findings)
}
