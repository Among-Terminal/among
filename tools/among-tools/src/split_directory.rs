//! Check that a terminal created by splitting starts in the directory of the
//! pane it was split from, and that an inherited directory which has become
//! unusable falls back with a report naming the requested location.
//!
//! Both facts live in a running window - a shell's own `pwd` is the only proof
//! that a terminal really started where the application intended - so this
//! drives the application and reads its self-check report back.

use crate::{
    sessions::{fresh, pair, pane_rectangles, Harness},
    Result,
};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Makes the shell publish OSC 7, which is how a pane's directory becomes known
/// after a `cd`. A terminal that never publishes one still knows the directory
/// it started in, which is what the split hands over.
const REPORT_DIRECTORY: &str =
    "export PROMPT_COMMAND='printf \"\\033]7;file://localhost%s\\007\" \"$PWD\"'";

/// What the application must say when a requested directory cannot be used.
const UNAVAILABLE: &str = "the requested directory is unavailable";

fn require(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

/// The directories the self-check last reported, as `(pane id, directory)`.
///
/// The report is a debug-printed list, so each entry is quoted and reads
/// `id:directory|uri`; a directory the application does not know is reported as
/// the word `unknown`, which is not a path and is kept verbatim so a caller
/// comparing it against a real directory fails.
fn reported_cwds(text: &str) -> Vec<(u64, String)> {
    let Some(start) = text.rfind("pane_cwds=[") else {
        return Vec::new();
    };
    let Some(tail) = text.get(start + "pane_cwds=[".len()..) else {
        return Vec::new();
    };
    let Some(list) = tail.split(']').next() else {
        return Vec::new();
    };
    list.split(',')
        .filter_map(|entry| {
            let entry = entry.trim().trim_matches('"');
            let (id, rest) = entry.split_once(':')?;
            let directory = rest.split('|').next().unwrap_or(rest).trim().to_string();
            Some((id.trim().parse().ok()?, directory))
        })
        .collect()
}

/// Whether two spellings name the same directory.
///
/// One terminal reports a path its shell published, which is the path as typed,
/// while another reports what the kernel resolved its start directory to. A
/// symlink anywhere in the check's own directory would otherwise make two panes
/// that really are in one directory look different, so both sides are resolved
/// before being compared.
fn same_directory(reported: &str, expected: &Path) -> bool {
    if reported == expected.to_string_lossy() {
        return true;
    }
    let resolved = |path: &Path| fs::canonicalize(path).ok();
    match (resolved(Path::new(reported)), resolved(expected)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// Wait until the self-check reports `directory` for `pane`, and return it.
fn reported_cwd(h: &mut Harness, pane: u64, wanted: &Path, phase: &str) -> Result<String> {
    let expected = wanted.to_string_lossy().to_string();
    let reported = h.wait(15, |this| {
        Ok(reported_cwds(&this.text()?)
            .iter()
            .any(|(id, directory)| *id == pane && same_directory(directory, wanted)))
    });
    match reported {
        Ok(()) => Ok(expected),
        Err(error) => {
            Err(format!("{phase}: terminal {pane} never reported {expected}: {error}").into())
        }
    }
}

/// The directory the shell in `pane` is actually in, read from that shell.
fn actual_pwd(h: &mut Harness, pane: u64, phase: &str) -> Result<String> {
    focus_pane(h, pane)?;
    let output = h.base.join(format!("{phase}-{pane}.pwd"));
    h.command(&format!("pwd > '{}'", output.display()))?;
    h.wait(15, |_| Ok(output.exists()))?;
    Ok(fs::read_to_string(&output)?.trim().to_string())
}

/// Click the body of a pane, which is how the window's focus follows the pointer.
fn focus_pane(h: &mut Harness, pane: u64) -> Result<()> {
    let geometry = h.geometry()?;
    let origin = pair(&geometry, "origin", ',')?;
    let bounds = pane_rectangles(&geometry, &pane.to_string())
        .last()
        .copied()
        .ok_or("pane has no native geometry")?;
    h.move_mouse(
        origin[0] + (bounds[0] + bounds[2] / 2.0) as i64,
        origin[1] + bounds[1] as i64 + 80.min(bounds[3] as i64 / 2),
    )?;
    h.xdo(&["click", "1"])?;
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}

/// Wait until the window shows `panes` terminals.
fn wait_for_panes(h: &mut Harness, panes: &str) -> Result<()> {
    h.wait(15, |this| {
        Ok(this.text()?.contains(&format!("panes={panes}")))
    })
}

/// Require that a terminal is where it was expected to start.
fn require_directory(phase: &str, pane: u64, reported: &str, expected: &Path) -> Result<()> {
    require(
        same_directory(reported, expected),
        &format!(
            "{phase}: terminal {pane} is in {reported}, not in {}",
            expected.display()
        ),
    )
}

/// The directory a snapshot records for one pane, if it records one.
fn snapshot_directory(h: &Harness, pane: u64) -> Option<String> {
    let snapshot = h.snapshot_if_written()?;
    snapshot["workspaces"][0]["directories"]
        .as_array()?
        .iter()
        .find(|entry| entry["pane"] == json!(pane))?["cwd"]
        .as_str()
        .map(str::to_string)
}

/// Wait until a pane's directory has been written into the snapshot.
fn wait_for_snapshot_directory(h: &mut Harness, pane: u64, directory: &Path) -> Result<()> {
    let expected = directory.to_string_lossy().to_string();
    let written = h.wait(15, |this| {
        Ok(snapshot_directory(this, pane)
            .is_some_and(|cwd| same_directory(&cwd, Path::new(&expected))))
    });
    match written {
        Ok(()) => Ok(()),
        Err(error) => Err(format!("terminal {pane} never recorded {expected}: {error}").into()),
    }
}

/// Restart the application on the session it wrote, and restore it.
fn restore(h: &mut Harness) -> Result<()> {
    h.stop(false)?;
    h.start("restore")?;
    h.wait(15, |this| {
        let text = this.text()?;
        Ok(text.contains("session_ready=false") && text.contains("Restore:"))
    })?;
    h.button("Restore")?;
    h.ready()
}

pub fn run(binary: &Path, scratch: &Path) -> Result<PathBuf> {
    let mut h = fresh(binary, scratch, "among-split-directory")?;
    h.env.insert("AMONG_SELFCHECK_TICKS".into(), "500".into());
    h.start("split")?;
    h.wait(15, |this| Ok(this.text()?.contains("has_focus=true")))?;
    // Directories to work in, inside the check's own directory so a failed run
    // leaves its evidence where the rest of the run's evidence is.
    let project = h.base.join("project");
    let other = h.base.join("other");
    fs::create_dir_all(&project)?;
    fs::create_dir_all(&other)?;

    // The first pane has to report its directory before a split can inherit it.
    h.command(REPORT_DIRECTORY)?;
    h.command(&format!("cd '{}'", project.display()))?;
    reported_cwd(&mut h, 1, &project, "cd")?;
    let offset = h.text()?.len();

    h.key("ctrl+shift+n")?;
    wait_for_panes(&mut h, "2")?;
    let inherited = reported_cwd(&mut h, 2, &project, "split")?;
    reported_cwd(&mut h, 1, &project, "split")?;
    require_directory("split", 2, &actual_pwd(&mut h, 2, "split")?, &project)?;
    require(
        !h.text()?
            .get(offset..)
            .unwrap_or_default()
            .contains(UNAVAILABLE),
        "an available inherited directory must not be reported as unavailable",
    )?;
    println!("PASS split terminal starts in {inherited}, as the pane it was split from");

    // Give the inherited terminal a directory of its own, so restoring the
    // session has two different locations to put back.
    focus_pane(&mut h, 2)?;
    h.command(REPORT_DIRECTORY)?;
    h.command(&format!("cd '{}'", other.display()))?;
    reported_cwd(&mut h, 2, &other, "cd")?;
    reported_cwd(&mut h, 1, &project, "cd")?;
    wait_for_snapshot_directory(&mut h, 1, &project)?;
    wait_for_snapshot_directory(&mut h, 2, &other)?;
    restore(&mut h)?;
    require_directory("restore", 1, &actual_pwd(&mut h, 1, "restore")?, &project)?;
    require_directory("restore", 2, &actual_pwd(&mut h, 2, "restore")?, &other)?;
    println!("PASS a session written after a split restores each pane into its own directory");

    // With that directory gone, a further split has to fall back and say so.
    fs::remove_dir_all(&other)?;
    let offset = h.text()?.len();
    h.key("ctrl+shift+n")?;
    wait_for_panes(&mut h, "3")?;
    let home = h.base.join("home");
    let fallback = reported_cwd(&mut h, 3, &home, "unavailable")?;
    require_directory(
        "unavailable",
        3,
        &actual_pwd(&mut h, 3, "unavailable")?,
        &home,
    )?;
    h.wait(15, |this| {
        Ok(this
            .text()?
            .get(offset..)
            .is_some_and(|text| text.contains(UNAVAILABLE)))
    })?;
    println!("PASS unusable inherited directory falls back to {fallback} and is reported");

    h.clean_log(&h.log)?;
    h.stop(false)?;
    println!("Evidence: {}", h.base.display());
    Ok(h.base.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = "among[tick7]: split_binding=F9 plugins=[] panes=2 focused_marked=true \
pane_cwds=[\"1:/home/dev/project|file:///home/dev/project\", \
\"2:/home/dev/project|file:///home/dev/project\"] last_notice=None";

    #[test]
    fn the_newest_report_is_read_rather_than_an_older_one() {
        let older = REPORT.replace("/home/dev/project", "/home/dev/other");
        let text = format!("{older}\n{REPORT}");
        assert_eq!(
            reported_cwds(&text),
            vec![
                (1, "/home/dev/project".to_string()),
                (2, "/home/dev/project".to_string())
            ]
        );
    }

    #[test]
    fn an_unknown_directory_is_not_mistaken_for_a_path() {
        let text = REPORT.replace(
            "2:/home/dev/project|file:///home/dev/project",
            "2:unknown|none",
        );
        assert_eq!(reported_cwds(&text)[1], (2, "unknown".to_string()));
        assert!(!same_directory("unknown", Path::new("/home/dev/project")));
    }

    #[test]
    fn a_missing_report_yields_nothing_rather_than_a_panic() {
        assert!(reported_cwds("among[tick1]: no panes yet").is_empty());
        assert!(reported_cwds("").is_empty());
    }

    #[test]
    fn identical_and_differently_spelled_paths_both_count_as_one_directory() {
        let directory = std::env::temp_dir().join("among-split-directory-test");
        let spelled = directory.to_string_lossy().to_string();
        assert!(same_directory(&spelled, &directory));
        assert!(same_directory("/tmp/../tmp", Path::new("/tmp")));
        assert!(!same_directory("/tmp", Path::new("/var")));
        let _ = fs::create_dir_all(&directory);
    }
}
