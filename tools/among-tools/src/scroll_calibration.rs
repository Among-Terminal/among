//! Check how scrolling moves a terminal's history.
//!
//! A wheel click must keep moving the history by the terminal library's own
//! amount, a touch surface gesture must move it by the distance the gesture
//! reports, and a program drawing a full screen must keep receiving the gesture
//! while the history stays put.
//!
//! The harness cannot produce a touch surface event — XTEST sends wheel clicks —
//! so the calibrated path is driven through the application's own handler by the
//! `scroll:` script step, and the wheel and full-screen paths by real clicks.
//! Nothing here decides what the application should do; every assertion is read
//! back out of a running window.

use crate::{
    sessions::{fresh, pair, pane_rectangles, Harness},
    Result,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How far a surface delta moves, in lines: GTK's own factor over the row
/// height. Stated here so a change to either the factor or the handler shows up
/// as a failing check rather than as a slower scroll nobody notices.
const SURFACE_FACTOR: f64 = 2.5;

/// The surface delta the scripted gesture reports, in surface pixels.
///
/// Negative, because that is the direction that moves the view towards older
/// output: a positive delta scrolls towards the newest row, which is where the
/// terminal already is.
const DELTA: f64 = -40.0;

/// The entries of one self-check list, as `(pane id, rest of the entry)`.
///
/// The report is a debug-printed list, so entries are quoted.
fn reported(text: &str, field: &str) -> Vec<(u64, String)> {
    let needle = format!("{field}=[");
    let Some(start) = text.rfind(&needle) else {
        return Vec::new();
    };
    let Some(tail) = text.get(start + needle.len()..) else {
        return Vec::new();
    };
    let Some(list) = tail.split(']').next() else {
        return Vec::new();
    };
    list.split(',')
        .filter_map(|entry| {
            let (id, rest) = entry.trim().trim_matches('"').split_once(':')?;
            Some((id.trim().parse().ok()?, rest.to_string()))
        })
        .collect()
}

/// One `key=value` field of a diagnostic line.
fn field(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest
        .find(|character: char| character.is_whitespace())
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// What the application's `scroll:` step reported for one scripted delta.
#[derive(Debug, Default, PartialEq)]
struct ScriptedScroll {
    moved: bool,
    cell: i32,
    from: f64,
    to: f64,
}

/// Every `scroll:` step the application reported, oldest first.
fn scripted_scrolls(text: &str) -> Vec<ScriptedScroll> {
    text.lines()
        .filter(|line| line.starts_with("among[scroll]:"))
        .filter_map(|line| {
            Some(ScriptedScroll {
                moved: field(line, "moved")? == "true",
                cell: field(line, "cell")?.parse().ok()?,
                from: field(line, "from")?.parse().ok()?,
                to: field(line, "to")?.parse().ok()?,
            })
        })
        .collect()
}

/// A pane's reported scroll position in lines and its row height in pixels.
fn scroll_of(h: &Harness, pane: u64) -> Option<(f64, i32)> {
    reported(&h.text().ok()?, "pane_scroll")
        .into_iter()
        .find(|(id, _)| *id == pane)
        .and_then(|(_, entry)| {
            let (lines, cell) = entry.split_once('@')?;
            Some((lines.parse().ok()?, cell.trim().parse().ok()?))
        })
}

/// The row count of the first visible pane, which is what the library derives
/// its wheel step from.
fn rows_of(h: &Harness) -> Option<u32> {
    let text = h.text().ok()?;
    let list = text
        .lines()
        .rfind(|line| line.contains("terminal_sizes=["))
        .and_then(|line| line.split_once("terminal_sizes=[").map(|(_, tail)| tail))?
        .split(']')
        .next()?;
    list.split_whitespace()
        .next()
        .and_then(|entry| entry.rsplit_once('x'))
        .and_then(|(_, rows)| rows.trim().parse().ok())
}

/// Wait until a pane's reported scroll position is at least `wanted` lines.
fn wait_for_scroll(h: &mut Harness, pane: u64, wanted: f64, phase: &str) -> Result<(f64, i32)> {
    let reached = h.wait(15, |this| {
        Ok(scroll_of(this, pane).is_some_and(|(lines, _)| lines >= wanted))
    });
    match reached {
        Ok(()) => Ok(scroll_of(h, pane).unwrap_or_default()),
        Err(error) => {
            Err(format!("{phase}: terminal {pane} never reached {wanted} lines: {error}").into())
        }
    }
}

/// Wait until a pane's reported scroll position is back at the bottom.
fn wait_for_bottom(h: &mut Harness, pane: u64, phase: &str) -> Result<(f64, i32)> {
    let settled = h.wait(15, |this| {
        Ok(scroll_of(this, pane).is_some_and(|(lines, _)| lines <= 0.01))
    });
    match settled {
        Ok(()) => Ok(scroll_of(h, pane).unwrap_or_default()),
        Err(error) => {
            Err(format!("{phase}: terminal {pane} never returned to the bottom: {error}").into())
        }
    }
}

/// Give the application a script to run at startup, one step per line.
fn script(h: &mut Harness, name: &str, steps: &[String]) -> Result<()> {
    let path = h.base.join(format!("{name}.script"));
    fs::write(&path, format!("{}\n", steps.join("\n")))?;
    h.env.insert("AMONG_SCRIPT".into(), path.into_os_string());
    Ok(())
}

/// Click the body of a pane, which is how the window's focus follows the
/// pointer, and run a command in it.
fn in_pane(h: &mut Harness, pane: u64, command: &str) -> Result<()> {
    // A layout change republishes every pane's rectangle; clicking before the new
    // one appears would aim at the previous position.
    h.wait(15, |this| {
        Ok(this
            .geometry()
            .is_ok_and(|geometry| !pane_rectangles(&geometry, &pane.to_string()).is_empty()))
    })?;
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
    h.command(command)
}

/// Write a full-screen program that records every byte it is sent, and return
/// the file it records into.
///
/// The program enters the alternate screen the way an editor or a pager does and
/// appends what it receives, so a gesture can be shown to reach the program
/// rather than moving the terminal's history.
fn full_screen_program(h: &Harness) -> Result<(PathBuf, PathBuf)> {
    let keys = h.base.join("full-screen-keys.txt");
    let program = h.base.join("full-screen.sh");
    fs::write(
        &program,
        format!(
            // Raw mode is what makes the gesture reach the program: in the
            // terminal's own line discipline an arrow key is echoed on screen
            // but held until a newline, so the program would never see it.
            // `tee -a` rather than `od` because od buffers a block at a time and
            // a short scroll would only appear once that buffer filled.
            "#!/bin/bash\nprintf '\\033[?1049h\\033[H'\nstty raw -echo\ncat | tee -a '{}'\nstty sane\n",
            keys.display()
        ),
    )?;
    fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))?;
    Ok((keys, program))
}

pub fn run(binary: &Path, scratch: &Path) -> Result<PathBuf> {
    let mut h = fresh(binary, scratch, "among-scroll")?;
    h.env.insert("AMONG_SELFCHECK_TICKS".into(), "900".into());

    // A surface delta with no history to move through must be handed back, which
    // is what keeps full-screen programs working.
    script(&mut h, "no-history", &[format!("scroll:1:{DELTA}")])?;
    h.start("no-history")?;
    h.wait(15, |this| {
        Ok(scripted_scrolls(&this.text()?)
            .iter()
            .any(|step| !step.moved))
    })?;
    wait_for_bottom(&mut h, 1, "no-history")?;
    println!("PASS a surface delta with no history is handed back to the library");

    // With history, the same deltas must move it by the distance they report.
    // The script creates the history first, because the steps run on a timer from
    // startup and there would otherwise be nothing to scroll.
    let steps = vec![
        "type:printf~'line~%s\\n'~$(seq~1~200)".to_string(),
        format!("scroll:1:{DELTA}"),
        format!("scroll:1:{DELTA}"),
        format!("scroll:1:{DELTA}"),
        format!("scroll:1:{DELTA}"),
    ];
    script(&mut h, "surface", &steps)?;
    h.start("surface")?;
    h.wait(30, |this| Ok(scripted_scrolls(&this.text()?).len() >= 4))?;
    let applied: Vec<ScriptedScroll> = scripted_scrolls(&h.text()?)
        .into_iter()
        .filter(|step| step.moved)
        .collect();
    if applied.len() != 4 {
        return Err(format!(
            "four surface deltas moved the history {0} times, not four",
            applied.len()
        )
        .into());
    }
    // Every step must move by exactly the reported distance converted to lines,
    // and the steps must accumulate rather than each starting over.
    let mut walked_back = 0.0;
    for (index, step) in applied.iter().enumerate() {
        let expected = DELTA * SURFACE_FACTOR / f64::from(step.cell);
        if (step.to - step.from - expected).abs() > 0.01 {
            return Err(format!(
                "step {} moved {:.2} lines, not the {expected:.2} a {DELTA} pixel delta reports over a {} pixel row",
                index + 1,
                step.to - step.from,
                step.cell
            )
            .into());
        }
        walked_back += step.from - step.to;
    }
    // The pane's own report measures the same movement, from the newest row.
    let (lines, _) = scroll_of(&h, 1).ok_or("no scroll report for the pane")?;
    if (lines - walked_back).abs() > 0.05 {
        return Err(format!(
            "the pane reports {lines} lines back, but the steps moved {walked_back}"
        )
        .into());
    }
    println!("PASS four surface deltas of {DELTA} moved the history {walked_back:.2} lines back");

    // A wheel click must keep moving the history by the library's own amount,
    // which is a tenth of the pane's rows per click.
    h.command("true")?;
    wait_for_bottom(&mut h, 1, "wheel")?;
    let rows = rows_of(&h).ok_or("terminal row count missing")?;
    // The library's own step: a tenth of the pane's rows per click, rounded up,
    // and never less than one line.
    let library = 3.0 * (f64::from(rows) / 10.0).ceil().max(1.0);
    h.xdo(&["click", "--repeat", "3", "--delay", "200", "4"])?;
    let (after_wheel, _) = wait_for_scroll(&mut h, 1, library / 2.0, "wheel")?;
    if (after_wheel - library).abs() > 0.5 {
        return Err(format!(
            "three wheel clicks over {rows} rows moved {after_wheel} lines, not the library's {library}"
        )
        .into());
    }
    println!("PASS three wheel clicks moved {after_wheel} lines, the library's own amount");
    h.xdo(&["click", "--repeat", "3", "--delay", "200", "5"])?;
    wait_for_bottom(&mut h, 1, "wheel-down")?;
    println!("PASS scrolling back with the wheel returns to the bottom");

    // A modified wheel click must reach the library like any other wheel click. The
    // X11 backend does not turn Shift into a horizontal event, so what this
    // proves is the weaker but real thing: the modified click moves the history
    // by the library's amount rather than by the calibrated surface step.
    h.command("true")?;
    wait_for_bottom(&mut h, 1, "shift-wheel")?;
    h.xdo(&["keydown", "shift"])?;
    h.xdo(&["click", "--repeat", "3", "--delay", "200", "4"])?;
    h.xdo(&["keyup", "shift"])?;
    let (after_shift, _) = wait_for_scroll(&mut h, 1, library / 2.0, "shift-wheel")?;
    if (after_shift - library).abs() > 0.5 {
        return Err(format!(
            "a modified wheel click moved {after_shift} lines, not the library's {library}"
        )
        .into());
    }
    println!("PASS a modified wheel click moved {after_shift} lines, the library's own amount");

    // Panes keep their own scroll position.
    h.key("ctrl+shift+n")?;
    h.wait(15, |this| Ok(this.text()?.contains("panes=2")))?;
    in_pane(&mut h, 1, "printf 'in one\\n'")?;
    h.xdo(&["click", "--repeat", "2", "--delay", "200", "4"])?;
    wait_for_scroll(&mut h, 1, 1.0, "pane-one")?;
    let second = scroll_of(&h, 2).map(|(lines, _)| lines);
    if second.is_some_and(|lines| lines > 0.01) {
        return Err(format!("the second pane moved to {second:?} lines of its own").into());
    }
    println!("PASS scrolling one pane left the second pane at {second:?} lines");

    // A program drawing a full screen receives the gesture, and the history does
    // not move. The second pane is used because it has no history to move
    // through, which is the state a full-screen program leaves the terminal in.
    let (keys, program) = full_screen_program(&h)?;
    in_pane(&mut h, 2, &format!("'{}'", program.display()))?;
    h.wait(15, |this| Ok(this.text()?.contains("panes=2")))?;
    std::thread::sleep(Duration::from_secs(1));
    h.xdo(&["click", "--repeat", "2", "--delay", "200", "4"])?;
    h.wait(15, |_| {
        Ok(keys.exists()
            && fs::read_to_string(&keys)
                .is_ok_and(|seen| seen.contains('\u{1b}') && seen.contains("[A")))
    })?;
    let during = scroll_of(&h, 2).map(|(lines, _)| lines).unwrap_or_default();
    if during > 0.01 {
        return Err("the history moved while a full-screen program owned the screen".into());
    }
    println!("PASS a full-screen program received the gesture and the history stayed put");

    h.clean_log(&h.log)?;
    h.stop(false)?;
    println!("Evidence: {}", h.base.display());
    Ok(h.base.clone())
}
