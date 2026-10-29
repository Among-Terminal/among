//! real pointer/key regression: mutating the model directly cannot prove that
//! GTK recognizes a drag, keeps its source alive, or returns terminal focus.

use crate::{
    sessions::{fresh, rectangles, Harness},
    Result,
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

fn require(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn field(text: &str, key: &str) -> Option<String> {
    let marker = format!(" {key}=");
    let (_, value) = text.rsplit_once(&marker)?;
    Some(value.split_whitespace().next()?.to_string())
}

fn order(h: &Harness) -> Result<Vec<u64>> {
    let text = field(&h.text()?, "workspace_order").ok_or("workspace order observation missing")?;
    text.trim_matches(['[', ']'])
        .split(',')
        .map(|part| part.parse().map_err(Into::into))
        .collect()
}

fn row(h: &Harness, index: usize) -> Result<[f64; 4]> {
    let text = h.text()?;
    let line = text
        .lines()
        .rfind(|line| line.contains("workspace_hit_rows="))
        .ok_or("row geometry missing")?;
    let rows = line
        .split_once("workspace_hit_rows=[")
        .ok_or("row list missing")?
        .1;
    rectangles(
        rows.split(']').next().ok_or("row list unclosed")?,
        &index.to_string(),
    )
    .first()
    .copied()
    .ok_or_else(|| format!("row {index} has no visible geometry").into())
}

fn move_to(h: &Harness, rect: [f64; 4], fraction: f64) -> Result<()> {
    h.move_mouse(
        (rect[0] + rect[2] * 0.4) as i64,
        (rect[1] + rect[3] * fraction) as i64,
    )
}

fn begin_drag(h: &mut Harness, source: usize) -> Result<()> {
    // a rebuild publishes model order before GTK has allocated the new rows.
    // wait for actual hit geometry rather than aiming at an unmapped widget.
    h.wait(10, |h| {
        Ok(row(h, source).is_ok_and(|rect| rect[2] > 32.0 && rect[3] > 16.0))
    })?;
    let rect = row(h, source)?;
    move_to(h, rect, 0.5)?;
    h.xdo(&["mousedown", "1"])?;
    for offset in [5.0, 12.0, 20.0] {
        h.move_mouse(
            (rect[0] + rect[2] * 0.4 + offset) as i64,
            (rect[1] + rect[3] * 0.5) as i64,
        )?;
        std::thread::sleep(Duration::from_millis(100));
    }
    h.wait(10, |h| {
        let text = h.text()?;
        Ok(
            field(&text, "workspace_drag").is_some_and(|value| value != "none")
                && field(&text, "workspace_native_drag").as_deref() == Some("true"),
        )
    })
}

fn hover(h: &mut Harness, destination: usize, after: bool) -> Result<()> {
    move_to(h, row(h, destination)?, if after { 0.75 } else { 0.25 })?;
    let gap = (destination + usize::from(after)).to_string();
    let indicator = format!("[{destination}:{}]", if after { "after" } else { "before" });
    h.wait(10, |h| {
        let text = h.text()?;
        Ok(field(&text, "workspace_target").as_deref() == Some(&gap)
            && field(&text, "workspace_indicators").as_deref() == Some(&indicator))
    })
}

fn drop_on(
    h: &mut Harness,
    source: usize,
    destination: usize,
    after: bool,
    expected: &[u64],
) -> Result<()> {
    let active = field(&h.text()?, "active_reference").ok_or("active reference missing")?;
    begin_drag(h, source)?;
    hover(h, destination, after)?;
    h.xdo(&["mouseup", "1"])?;
    h.wait(10, |h| {
        let text = h.text()?;
        Ok(order(h)? == expected
            && field(&text, "workspace_drag").as_deref() == Some("none")
            && field(&text, "workspace_indicators").as_deref() == Some("[]")
            && field(&text, "active_reference").as_deref() == Some(active.as_str())
            && field(&text, "has_focus").as_deref() == Some("true"))
    })
}

fn active(h: &mut Harness, key: &str, reference: u64) -> Result<()> {
    h.key(key)?;
    let expected = reference.to_string();
    h.wait(10, |h| {
        Ok(field(&h.text()?, "active_reference").as_deref() == Some(expected.as_str()))
    })
}

fn viewport(h: &Harness) -> Result<[f64; 4]> {
    let text = field(&h.text()?, "workspace_viewport").ok_or("viewport missing")?;
    rectangles(&text, "viewport")
        .first()
        .copied()
        .ok_or_else(|| "viewport geometry missing".into())
}

fn pane_counts(h: &Harness) -> Result<Vec<usize>> {
    let text = h.text()?;
    let line = text
        .lines()
        .rfind(|line| line.contains("sidebar_states=["))
        .ok_or("sidebar states missing")?;
    let states = line
        .split_once("sidebar_states=[")
        .ok_or("sidebar states missing")?
        .1;
    states
        .split(']')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .map(|entry| {
            entry
                .rsplit_once(':')
                .ok_or_else(|| "sidebar state has no count".into())
                .and_then(|(_, count)| count.parse().map_err(Into::into))
        })
        .collect()
}

fn pane_drag_start(h: &Harness, pane: &str) -> Result<(i64, i64)> {
    let geometry = h.geometry()?;
    let origin = crate::sessions::pair(&geometry, "origin", ',')?;
    let rect = crate::sessions::pane_rectangles(&geometry, pane)
        .first()
        .copied()
        .ok_or_else(|| format!("pane {pane} header geometry missing"))?;
    Ok((
        origin[0] + rect[0] as i64 + 120,
        origin[1] + rect[1] as i64 + 10,
    ))
}

fn begin_pane_drag(h: &mut Harness, pane: &str) -> Result<()> {
    let (x, y) = pane_drag_start(h, pane)?;
    h.move_mouse(x, y)?;
    h.xdo(&["mousedown", "1"])?;
    for offset in [8, 16, 24] {
        h.move_mouse(x + offset, y)?;
        std::thread::sleep(Duration::from_millis(100));
    }
    h.wait(10, |h| {
        Ok(field(&h.text()?, "pane_drag").is_some_and(|value| value != "none"))
    })
}

fn hover_pane(h: &mut Harness, destination: usize) -> Result<()> {
    move_to(h, row(h, destination)?, 0.5)?;
    let reference = order(h)?
        .get(destination)
        .ok_or("pane destination is outside the workspace order")?
        .to_string();
    let indicator = format!("[{destination}]");
    h.wait(10, |h| {
        let text = h.text()?;
        Ok(
            field(&text, "pane_target").as_deref() == Some(reference.as_str())
                && field(&text, "pane_indicators").as_deref() == Some(indicator.as_str()),
        )
    })
}

fn drop_pane_on(h: &mut Harness, pane: &str, destination: usize) -> Result<()> {
    begin_pane_drag(h, pane)?;
    hover_pane(h, destination)?;
    h.xdo(&["mouseup", "1"])?;
    h.wait(10, |h| {
        let text = h.text()?;
        Ok(field(&text, "pane_drag").as_deref() == Some("none")
            && field(&text, "pane_indicators").as_deref() == Some("[]"))
    })
}

fn reject_pane_drop(h: &mut Harness) -> Result<()> {
    let before = order(h)?;
    let before_counts = pane_counts(h)?;
    let active_reference = field(&h.text()?, "active_reference")
        .ok_or("active reference missing")?
        .parse::<u64>()?;
    let source_row = before
        .iter()
        .position(|reference| *reference == active_reference)
        .ok_or("active workspace is not in the order")?;
    let destination = if source_row == 0 { 1 } else { 0 };

    // Dropping on the source entry is a no-op for both the pane and the order.
    begin_pane_drag(h, "4")?;
    move_to(h, row(h, source_row)?, 0.5)?;
    h.wait(10, |h| {
        let text = h.text()?;
        Ok(field(&text, "pane_target").as_deref() == Some("none")
            && field(&text, "pane_indicators").as_deref() == Some("[]"))
    })?;
    h.xdo(&["mouseup", "1"])?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "pane_drag").as_deref() == Some("none"))
    })?;
    require(
        order(h)? == before
            && pane_counts(h)? == before_counts
            && field(&h.text()?, "active_reference").as_deref()
                == Some(active_reference.to_string().as_str()),
        "a pane dropped on its own workspace changed the model",
    )?;

    // A different entry receives the pane, but the workspace order stays put.
    drop_pane_on(h, "4", destination)?;
    let destination_reference = before[destination].to_string();
    h.wait(10, |h| {
        let counts = pane_counts(h)?;
        Ok(order(h)? == before
            && counts.get(destination).copied()
                == before_counts.get(destination).map(|count| count + 1)
            && counts.get(source_row).copied()
                == before_counts.get(source_row).map(|count| count - 1)
            && field(&h.text()?, "active_reference").as_deref()
                == Some(destination_reference.as_str()))
    })?;
    require(
        field(&h.text()?, "focused").as_deref() == Some("PaneId(4)"),
        "a pane drop did not focus the moved pane",
    )?;

    // Put the fixture back where later reorder and session assertions expect it.
    drop_pane_on(h, "4", source_row)?;
    let expected_active = active_reference.to_string();
    h.wait(10, |h| {
        Ok(order(h)? == before
            && pane_counts(h)? == before_counts
            && field(&h.text()?, "active_reference").as_deref() == Some(expected_active.as_str()))
    })?;
    // The split added pane 4 and focused it; remove it again so the workspace's
    // birth pane and therefore its restored reference stay unchanged.
    h.key("ctrl+shift+w")?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "panes").as_deref() == Some("1")
            && field(&h.text()?, "focused").as_deref() == Some("PaneId(3)")
            && pane_counts(h)?.iter().sum::<usize>() == 3)
    })?;
    println!("PASS a pane drag moves the pane without reordering workspaces");
    Ok(())
}

fn editing(h: &Harness, index: usize) -> Result<bool> {
    let text = h.text()?;
    let line = text
        .lines()
        .rfind(|line| line.contains("sidebar_states=["))
        .ok_or("sidebar states missing")?;
    let states = line
        .split_once("sidebar_states=[")
        .ok_or("sidebar states missing")?
        .1;
    Ok(states
        .split(']')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .any(|entry| {
            let mut parts = entry.split(':');
            parts.next() == Some(index.to_string().as_str()) && parts.nth(1) == Some("true")
        }))
}

fn rename(h: &mut Harness, index: usize, name: &str) -> Result<()> {
    std::thread::sleep(Duration::from_millis(450));
    h.wait(10, |h| {
        Ok(row(h, index).is_ok_and(|rect| rect[2] > 32.0 && rect[3] > 16.0))
    })?;
    move_to(h, row(h, index)?, 0.5)?;
    h.xdo(&["click", "--repeat", "2", "--delay", "80", "1"])?;
    h.wait(10, |h| editing(h, index))?;
    h.key("ctrl+a")?;
    h.xdo(&["type", "--clearmodifiers", name])?;
    h.key("Return")?;
    h.wait(10, |h| Ok(!editing(h, index)?))
}

fn long_list_and_identical_rows(h: &mut Harness) -> Result<()> {
    h.forget_session()?;
    h.env.insert("AMONG_COLOR_SCHEME".into(), "dark".into());
    h.env
        .insert("AMONG_REBIND".into(), "workspace-2=<Control><Alt>g".into());
    h.start("scroll-and-duplicates")?;
    h.ready()?;
    h.key("alt+F10")?;
    h.xdo(&["windowsize", &h.window, "900", "300"])?;
    h.wait(10, |h| {
        Ok(viewport(h)?[3] > 100.0 && viewport(h)?[3] < 300.0)
    })?;
    for count in 2..=12 {
        h.key("ctrl+shift+t")?;
        h.wait(10, |h| Ok(order(h)?.len() == count))?;
    }
    for count in 13..=24 {
        h.key("ctrl+shift+t")?;
        h.wait(10, |h| Ok(order(h)?.len() == count))?;
    }
    for escape in [true, false] {
        let bounds = viewport(h)?;
        move_to(h, bounds, 0.5)?;
        h.xdo(&["click", "--repeat", "30", "--delay", "10", "4"])?;
        h.wait(10, |h| {
            Ok(field(&h.text()?, "workspace_scroll").as_deref() == Some("0"))
        })?;
        begin_drag(h, 0)?;
        let last = row(h, 23)?;
        let scroll_limit = last[1] + last[3] - bounds[1] - bounds[3];
        h.move_mouse(
            (bounds[0] + bounds[2] * 0.4) as i64,
            (bounds[1] + bounds[3] - 4.0) as i64,
        )?;
        h.wait(3, |h| {
            let scroll: f64 = field(&h.text()?, "workspace_scroll")
                .ok_or("scroll observation missing")?
                .parse()?;
            Ok(scroll > 0.0 && scroll < scroll_limit - 12.0)
        })?;
        if escape {
            h.xdo(&["key", "Escape"])?;
        } else {
            h.move_mouse(
                (bounds[0] + bounds[2] + 100.0) as i64,
                (bounds[1] + 50.0) as i64,
            )?;
        }
        h.wait(3, |h| {
            let text = h.text()?;
            Ok(
                field(&text, "workspace_indicators").as_deref() == Some("[]")
                    && field(&text, "workspace_target").as_deref() == Some("none"),
            )
        })?;
        let stopped = field(&h.text()?, "workspace_scroll").ok_or("stopped adjustment missing")?;
        let stopped_value: f64 = stopped.parse()?;
        require(
            stopped_value > 0.0 && stopped_value < scroll_limit,
            "cleanup observation reached a scroll limit",
        )?;
        std::thread::sleep(Duration::from_millis(750));
        let observation_boundary = h.text()?.len();
        let mut later = None;
        h.wait(3, |h| {
            later = field(
                h.text()?.get(observation_boundary..).unwrap_or(""),
                "workspace_scroll",
            );
            Ok(later.is_some())
        })?;
        require(
            later.as_deref() == Some(stopped.as_str()),
            "drag scrolling continued after cancellation or leave",
        )?;
        h.xdo(&["mouseup", "1"])?;
        h.wait(3, |h| {
            Ok(field(&h.text()?, "workspace_drag").as_deref() == Some("none"))
        })?;
        require(
            order(h)? == (1..=24).collect::<Vec<_>>(),
            "mid-scroll cancellation or leave reordered workspaces",
        )?;
    }
    println!("PASS Escape and leaving the sidebar stop drag scrolling in the middle of its range");
    for remaining in (12..24).rev() {
        h.key("ctrl+shift+w")?;
        h.wait(10, |h| Ok(order(h)?.len() == remaining))?;
    }
    let shells = fs::read_to_string(h.base.join("shell-launches"))?;
    let bounds = viewport(h)?;
    move_to(h, bounds, 0.5)?;
    h.xdo(&["click", "--repeat", "20", "--delay", "15", "4"])?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "workspace_scroll").as_deref() == Some("0"))
    })?;
    begin_drag(h, 0)?;
    hover(h, 1, false)?;
    h.move_mouse(
        (bounds[0] + bounds[2] * 0.4) as i64,
        (bounds[1] + bounds[3] - 4.0) as i64,
    )?;
    h.wait(10, |h| {
        let end = row(h, 11)?;
        Ok(end[1] >= bounds[1] && end[1] + end[3] <= bounds[1] + bounds[3] + 1.0)
    })?;
    hover(h, 11, true)?;
    h.screenshot("dark-scroll-preview.png")?;
    h.xdo(&["mouseup", "1"])?;
    let expected: Vec<_> = (2..=12).chain([1]).collect();
    h.wait(10, |h| {
        Ok(order(h)? == expected && field(&h.text()?, "workspace_drag").as_deref() == Some("none"))
    })?;
    move_to(h, bounds, 0.5)?;
    h.xdo(&["click", "--repeat", "20", "--delay", "15", "5"])?;
    h.wait(10, |h| {
        Ok(row(h, 11).is_ok_and(|row| row[1] + row[3] <= bounds[1] + bounds[3] + 1.0))
    })?;
    begin_drag(h, 11)?;
    h.move_mouse(
        (bounds[0] + bounds[2] * 0.4) as i64,
        (bounds[1] + 4.0) as i64,
    )?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "workspace_scroll").as_deref() == Some("0"))
    })?;
    h.xdo(&["key", "Escape"])?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "workspace_drag").as_deref() == Some("none"))
    })?;
    h.xdo(&["mouseup", "1"])?;
    require(
        order(h)? == expected,
        "upward drag scrolling committed a cancelled move",
    )?;
    move_to(h, bounds, 0.5)?;
    h.xdo(&["click", "--repeat", "20", "--delay", "15", "5"])?;
    active(h, "alt+9", 10)?;
    active(h, "ctrl+alt+g", 3)?;
    rename(h, 11, "same project")?;
    rename(h, 7, "same project")?;
    drop_on(h, 11, 7, false, &[2, 3, 4, 5, 6, 7, 8, 1, 9, 10, 11, 12])?;
    drop_on(h, 7, 8, true, &[2, 3, 4, 5, 6, 7, 8, 9, 1, 10, 11, 12])?;
    drop_on(h, 8, 7, false, &[2, 3, 4, 5, 6, 7, 8, 1, 9, 10, 11, 12])?;
    require(
        fs::read_to_string(h.base.join("shell-launches"))? == shells,
        "scrolling, rename or duplicate-row moves restarted shells",
    )?;
    h.clean_log(&h.log)?;
    h.stop(false)?;
    println!("PASS long-list scrolling, ninth-position selection, custom positional binding and equal-looking row callbacks");
    Ok(())
}

pub fn run(binary: &Path, scratch: &Path) -> Result<PathBuf> {
    let mut h = fresh(binary, scratch, "among-workspace-reorder")?;
    h.env.insert("AMONG_SELFCHECK_TICKS".into(), "2400".into());
    h.env.insert("AMONG_COLOR_SCHEME".into(), "light".into());
    h.start("pointer")?;
    h.ready()?;
    for count in 2..=3 {
        h.key("ctrl+shift+t")?;
        h.wait(10, |h| Ok(order(h)?.len() == count))?;
    }
    let shells = fs::read_to_string(h.base.join("shell-launches"))?;
    let focused_pid = shells
        .lines()
        .last()
        .ok_or("focused shell missing")?
        .to_string();
    drop_on(&mut h, 0, 2, true, &[2, 3, 1])?;
    drop_on(&mut h, 2, 0, false, &[1, 2, 3])?;
    drop_on(&mut h, 1, 1, false, &[1, 2, 3])?;
    require(
        fs::read_to_string(h.base.join("shell-launches"))? == shells,
        "reordering restarted a shell",
    )?;
    let proof = h.base.join("focus-proof");
    h.command(&format!("printf '%s' \"$$\" > '{}'", proof.display()))?;
    h.wait(10, |_| Ok(proof.is_file()))?;
    require(
        fs::read_to_string(proof)? == focused_pid,
        "typing did not reach the previously focused shell",
    )?;
    println!("PASS pointer insertion in both directions and no-op preserve active identity and shell processes");
    // Give the active workspace a second pane so a pane move does not close it;
    // this keeps the fixture's later workspace and session assertions stable.
    h.key("ctrl+shift+n")?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "panes").as_deref() == Some("2"))
    })?;
    reject_pane_drop(&mut h)?;
    begin_drag(&mut h, 0)?;
    let bounds = viewport(&h)?;
    h.move_mouse(
        (bounds[0] + bounds[2] + 100.0) as i64,
        (bounds[1] + 50.0) as i64,
    )?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "workspace_indicators").as_deref() == Some("[]"))
    })?;
    h.xdo(&["mouseup", "1"])?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "workspace_drag").as_deref() == Some("none"))
    })?;
    require(
        order(&h)? == [1, 2, 3],
        "an out-of-sidebar drop committed a move",
    )?;

    begin_drag(&mut h, 0)?;
    hover(&mut h, 2, true)?;
    h.screenshot("insertion-preview.png")?;
    // keep the held pointer button intact while Escape reaches the native drag.
    h.xdo(&["key", "Escape"])?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "workspace_drag").as_deref() == Some("none"))
    })?;
    h.xdo(&["mouseup", "1"])?;
    require(order(&h)? == [1, 2, 3], "Escape committed a reorder")?;
    println!("PASS Escape cancels the native drag");

    active(&mut h, "alt+1", 1)?;
    drop_on(&mut h, 2, 0, false, &[3, 1, 2])?;
    active(&mut h, "alt+1", 3)?;
    active(&mut h, "alt+2", 1)?;
    active(&mut h, "alt+9", 1)?;
    active(&mut h, "ctrl+Next", 2)?;
    active(&mut h, "ctrl+Next", 3)?;
    active(&mut h, "ctrl+Prior", 2)?;
    h.button("toggle")?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "sidebar_collapsed").as_deref() == Some("true"))
    })?;
    drop_on(&mut h, 2, 0, false, &[2, 3, 1])?;
    active(&mut h, "alt+1", 2)?;
    h.screenshot("collapsed-order.png")?;
    println!("PASS Alt+digit and cycling follow the user order in both sidebar presentations");

    h.wait(10, |h| {
        Ok(h.snapshot_if_written().is_some_and(|snapshot| {
            snapshot["workspaces"][0]["name"] == "project 2"
                && snapshot["workspaces"][1]["name"] == "project 3"
                && snapshot["workspaces"][2]["name"] == "project"
                && snapshot["active"] == 0
        }))
    })?;
    h.stop(true)?;
    h.start("restored")?;
    h.button("Restore")?;
    h.ready()?;
    require(order(&h)? == [2, 3, 1], "restoration lost manual ordering")?;
    active(&mut h, "alt+3", 1)?;
    let restored_shell = fs::read_to_string(h.base.join("shell-launches"))?
        .lines()
        .last()
        .ok_or("restored shell missing")?
        .to_string();
    let restored_proof = h.base.join("restored-focus-proof");
    h.command(&format!(
        "printf '%s' \"$$\" > '{}'",
        restored_proof.display()
    ))?;
    h.wait(10, |_| Ok(restored_proof.is_file()))?;
    require(
        fs::read_to_string(restored_proof)? == restored_shell,
        "restored Alt+3 did not route input to that workspace's shell",
    )?;
    // Hold the preview while a real cwd change reaches a periodic checkpoint.
    // Releasing immediately afterwards leaves almost the full checkpoint period
    // for native close/consent, without waiting for another self-check tick.
    let exit_directory = h.base.join("exit-before-checkpoint");
    fs::create_dir(&exit_directory)?;
    h.command(&format!(
        "sleep 2; cd '{}'; printf '\\033]7;file://localhost%s\\007' \"$PWD\"",
        exit_directory.display()
    ))?;
    begin_drag(&mut h, 2)?;
    hover(&mut h, 0, false)?;
    h.wait(10, |h| {
        Ok(h.snapshot_if_written().is_some_and(|snapshot| {
            snapshot["workspaces"].as_array().is_some_and(|workspaces| {
                workspaces.iter().any(|workspace| {
                    workspace["directories"]
                        .as_array()
                        .is_some_and(|directories| {
                            directories.iter().any(|directory| {
                                directory["cwd"].as_str() == exit_directory.to_str()
                            })
                        })
                })
            })
        }))
    })?;
    let before_exit = h
        .snapshot_if_written()
        .ok_or("pre-exit checkpoint missing")?;
    require(
        before_exit["workspaces"][0]["name"] == "project 2",
        "pre-exit checkpoint already has the final order",
    )?;
    let boundary = h.text()?.len();
    h.xdo(&["mouseup", "1", "key", "alt+F4"])?;
    h.wait(3, |h| {
        Ok(rectangles(h.text()?.get(boundary..).unwrap_or(""), "Exit")
            .last()
            .is_some_and(|rect| rect[2] > 0.0 && rect[3] > 0.0))
    })?;
    require(
        h.snapshot_if_written().as_ref() == Some(&before_exit),
        "periodic checkpoint saved the drop before confirmed exit",
    )?;
    let exit = rectangles(h.text()?.get(boundary..).unwrap_or(""), "Exit")
        .last()
        .copied()
        .ok_or("Exit geometry missing")?;
    h.move_mouse(
        (exit[0] + exit[2] / 2.0) as i64,
        (exit[1] + exit[3] / 2.0) as i64,
    )?;
    h.xdo(&["click", "1"])?;
    let status = h
        .process
        .take()
        .ok_or("application process missing")?
        .wait(Duration::from_secs(15))?;
    require(
        status.success(),
        "confirmed exit did not terminate normally",
    )?;
    let text = h.text()?;
    let closing = text.get(boundary..).ok_or("closing observations missing")?;
    require(
        !closing.contains("checkpoint_submitted=true") && closing.contains("final_saved=true"),
        "new order was not exclusively saved by the final-write completion path",
    )?;
    let final_snapshot = h.snapshot_if_written().ok_or("final snapshot missing")?;
    require(
        final_snapshot["workspaces"][0]["name"] == "project" && final_snapshot["active"] == 0,
        "normal final save lost the moved workspace or active position",
    )?;
    h.clean_log(&h.log)?;
    println!("PASS actual checkpoint, explicit Restore, and final save retain order and active position; evidence: {}", h.base.display());
    long_list_and_identical_rows(&mut h)?;
    Ok(h.base.clone())
}
