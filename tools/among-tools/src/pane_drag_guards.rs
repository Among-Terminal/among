//! Live guards around pane moves that need a second program.
//!
//! Foreign data: a separate X11 client drags terminal text, a file and an
//! opaque number onto Among's sidebar. None of them may be shown as a pane
//! destination, start a workspace reorder, or change workspaces when released.
//!
//! Plugin refresh: a plugin changes a workspace summary while a pane header is
//! being dragged. The sidebar must keep the rows, and the destination shown on
//! them, until the drag ends, and then draw the new summary.

use crate::{
    pane_workspace_move::{
        begin_pane_drag, field, hover_pane, move_to, order, pane_counts, require, row, settle,
        wait_no_pane_drag,
    },
    process::OwnedProcess,
    sessions::{fresh, Harness},
    Result,
};
use serde_json::json;
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    time::Duration,
};

const SOURCE_TITLE: &str = "Among foreign drag source";

/// Window geometry of the foreign source, in screen coordinates.
fn source_geometry(h: &Harness) -> Result<[i64; 4]> {
    let output = h.probe(
        "/usr/bin/xdotool",
        &[
            "search",
            "--name",
            SOURCE_TITLE,
            "getwindowgeometry",
            "--shell",
        ],
    )?;
    let value = |key: &str| -> Result<i64> {
        Ok(output
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .ok_or_else(|| format!("drag source geometry has no {key}"))?
            .trim()
            .parse()?)
    };
    Ok([value("X")?, value("Y")?, value("WIDTH")?, value("HEIGHT")?])
}

/// Drag one foreign payload from the source window onto a workspace entry.
fn foreign_drop(
    h: &mut Harness,
    source: &Path,
    runner_home: &str,
    kind: &str,
    destination: usize,
) -> Result<()> {
    let ready = h.base.join(format!("drag-source-{kind}.ready"));
    let file = h.base.join("foreign-file.txt");
    fs::write(&file, b"foreign file")?;
    let log = File::create(h.base.join(format!("drag-source-{kind}.log")))?;
    let args = [
        runner_home,
        kind,
        file.to_str().ok_or("fixture path is not UTF-8")?,
        ready.to_str().ok_or("fixture path is not UTF-8")?,
    ];
    let mut process = OwnedProcess::spawn(source, &args, &h.env, &h.base, &log)?;
    h.wait(10, |_| Ok(ready.exists()))?;
    // Keep the source clear of the sidebar so the entry under the pointer is
    // Among's, not the source window covering it.
    h.xdo(&["search", "--name", SOURCE_TITLE, "windowmove", "900", "500"])?;
    std::thread::sleep(Duration::from_millis(500));
    let [x, y, width, height] = source_geometry(h)?;
    let (start_x, start_y) = (x + width / 2, y + height / 2);
    let before = order(h)?;
    let counts = pane_counts(h)?;
    let active = field(&h.text()?, "active_reference");

    h.move_mouse(start_x, start_y)?;
    h.xdo(&["mousedown", "1"])?;
    for offset in [8, 16, 24] {
        h.move_mouse(start_x + offset, start_y)?;
        std::thread::sleep(Duration::from_millis(100));
    }
    // Every later assertion is about something not happening, so the native
    // drag itself must be shown to have started; otherwise a fixture that never
    // dragged would pass.
    let began = PathBuf::from(format!("{}.began", ready.display()));
    let ended = PathBuf::from(format!("{}.ended", ready.display()));
    if h.wait(10, |_| Ok(began.exists())).is_err() {
        h.xdo(&["mouseup", "1"])?;
        process.cleanup()?;
        return Err(format!("the foreign {kind} drag never started").into());
    }
    move_to(h, row(h, destination)?, 0.5)?;
    std::thread::sleep(Duration::from_millis(300));
    // Small movements over the entry deliver motion events to its drop targets.
    let target = row(h, destination)?;
    for fraction in [0.4, 0.6, 0.5] {
        move_to(h, target, fraction)?;
        std::thread::sleep(Duration::from_millis(150));
    }
    settle(h)?;
    let text = h.text()?;
    let hovering = [
        ("pane_drag", "none"),
        ("pane_target", "none"),
        ("pane_indicators", "[]"),
        ("workspace_drag", "none"),
        ("workspace_target", "none"),
        ("workspace_indicators", "[]"),
    ];
    let mut failures: Vec<String> = hovering
        .iter()
        .filter_map(|(key, expected)| {
            let actual = field(&text, key);
            (actual.as_deref() != Some(*expected))
                .then(|| format!("a foreign {kind} drag over an entry reported {key}={actual:?}"))
        })
        .collect();
    // The release and the source's exit happen whatever was seen, so one
    // failing payload does not leave the pointer held for the next.
    h.xdo(&["mouseup", "1"])?;
    if h.wait(10, |_| Ok(ended.exists())).is_err() {
        failures.push(format!(
            "the foreign {kind} drag did not finish after release"
        ));
    }
    settle(h)?;
    if order(h)? != before
        || pane_counts(h)? != counts
        || field(&h.text()?, "active_reference") != active
    {
        failures.push(format!("a foreign {kind} drop changed the workspaces"));
    }
    process.cleanup()?;
    require(failures.is_empty(), &failures.join("; "))
}

fn foreign_phase(binary: &Path, source: &Path, scratch: &Path) -> Result<()> {
    let runner_home = std::env::var("HOME")?;
    let mut h = fresh(binary, scratch, "among-pane-foreign-drops")?;
    h.env.insert("AMONG_SELFCHECK_TICKS".into(), "2400".into());
    h.start("foreign")?;
    h.ready()?;
    // Two workspaces, so there is an entry other than the active one to aim at.
    h.key("ctrl+shift+n")?;
    h.wait(10, |h| Ok(pane_counts(h)? == [2]))?;
    h.key("ctrl+shift+t")?;
    h.wait(10, |h| Ok(order(h)?.len() == 2))?;
    // Every payload is tried before the phase fails, so a report names each
    // kind that was accepted rather than only the first.
    let mut failures = Vec::new();
    for kind in ["text", "file", "u64"] {
        match foreign_drop(&mut h, source, &runner_home, kind, 0) {
            Ok(()) => println!(
                "PASS a foreign {kind} drag is neither a pane nor a workspace and changes nothing"
            ),
            Err(error) => {
                println!("FAIL {error}");
                failures.push(error.to_string());
            }
        }
    }
    require(failures.is_empty(), &failures.join("; "))?;
    h.clean_log(&h.log)?;
    h.stop(false)?;
    Ok(())
}

fn rendered(h: &Harness) -> Result<String> {
    Ok(field(&h.text()?, "rendered_summaries").unwrap_or_default())
}

fn stored(h: &Harness) -> Result<String> {
    Ok(field(&h.text()?, "stored_summaries").unwrap_or_default())
}

fn refresh_phase(binary: &Path, plugin: &Path, scratch: &Path) -> Result<()> {
    let mut h = fresh(binary, scratch, "among-pane-drag-refresh")?;
    let plugins = h.base.join("data/among/plugins");
    fs::create_dir_all(&plugins)?;
    fs::copy(plugin, plugins.join("summaries"))?;
    let summary = plugins.join("summary.txt");
    fs::write(&summary, b"before-drag")?;
    fs::write(
        h.base.join("config/among/plugins.json"),
        serde_json::to_vec(&json!({"plugins":[{"name":"summaries","enabled":true}]}))?,
    )?;
    h.env.insert("AMONG_SELFCHECK_TICKS".into(), "2400".into());
    h.start("refresh")?;
    h.ready()?;
    h.key("ctrl+shift+n")?;
    h.wait(10, |h| Ok(pane_counts(h)? == [2]))?;
    h.key("ctrl+shift+t")?;
    h.wait(10, |h| Ok(order(h)?.len() == 2))?;
    h.key("alt+1")?;
    h.wait(15, |h| {
        let rendered = rendered(h)?;
        Ok(rendered.contains("0:before-drag") && rendered.contains("1:before-drag"))
    })?;

    // Drag pane 2 over the second entry, then let the plugin change the
    // summaries while the destination is shown.
    begin_pane_drag(&mut h, "2")?;
    hover_pane(&mut h, 1)?;
    fs::write(&summary, b"during-drag")?;
    h.wait(15, |h| Ok(stored(h)?.contains("1:during-drag")))?;
    settle(&mut h)?;
    let text = h.text()?;
    require(
        field(&text, "pane_indicators").as_deref() == Some("[1]")
            && field(&text, "pane_target").as_deref()
                == order(&h)?.get(1).map(ToString::to_string).as_deref(),
        "a plugin refresh during the drag took away the shown destination",
    )?;
    require(
        !rendered(&h)?.contains("during-drag"),
        "a plugin refresh rebuilt the sidebar during the drag",
    )?;
    h.xdo(&["mouseup", "1"])?;
    wait_no_pane_drag(&mut h)?;
    h.wait(10, |h| Ok(pane_counts(h)? == [1, 2]))?;
    // The held refresh is drawn once the drag is over.
    h.wait(15, |h| Ok(rendered(h)?.contains("during-drag")))?;
    println!("PASS a plugin refresh during a pane drag keeps the destination and is drawn after the drop");

    // The same hold must be released when the drag is cancelled instead of
    // dropped: no move happens, so nothing else forces the sidebar to rebuild.
    let counts = pane_counts(&h)?;
    begin_pane_drag(&mut h, "2")?;
    hover_pane(&mut h, 0)?;
    fs::write(&summary, b"cancelled-drag")?;
    h.wait(15, |h| Ok(stored(h)?.contains("0:cancelled-drag")))?;
    settle(&mut h)?;
    require(
        !rendered(&h)?.contains("cancelled-drag"),
        "a plugin refresh rebuilt the sidebar during the cancelled drag",
    )?;
    h.xdo(&["key", "Escape"])?;
    wait_no_pane_drag(&mut h)?;
    h.xdo(&["mouseup", "1"])?;
    settle(&mut h)?;
    require(
        pane_counts(&h)? == counts,
        "the cancelled pane drag moved a pane",
    )?;
    h.wait(15, |h| Ok(rendered(h)?.contains("cancelled-drag")))?;
    println!("PASS a plugin refresh held during a cancelled pane drag is drawn after the cancel");
    h.clean_log(&h.log)?;
    h.stop(false)?;
    Ok(())
}

pub fn run(binary: &Path, source: &Path, plugin: &Path, scratch: &Path) -> Result<PathBuf> {
    // The fixtures are started from each check's own directory, so a path
    // given relative to the caller's directory would no longer resolve.
    let source = fs::canonicalize(source)
        .map_err(|error| format!("drag source {}: {error}", source.display()))?;
    let plugin = fs::canonicalize(plugin)
        .map_err(|error| format!("summary plugin {}: {error}", plugin.display()))?;
    foreign_phase(binary, &source, scratch)?;
    refresh_phase(binary, &plugin, scratch)?;
    Ok(scratch.to_path_buf())
}
