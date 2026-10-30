//! real pointer regression: a pane header drag onto a workspace entry must move
//! the live terminal without reordering workspaces, and the move must survive a
//! saved session. Driving the model directly would not prove GTK recognizes the
//! pane payload, keeps the drag source alive, or restores the destination.

use crate::{
    sessions::{fresh, rectangles, Harness},
    Result,
};
use serde_json::Value;
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

/// The number of the newest periodic self-check report in the log.
fn last_tick(text: &str) -> u64 {
    text.lines()
        .filter_map(|line| line.strip_prefix("among[tick"))
        .filter_map(|rest| rest.split(']').next())
        .filter_map(|number| number.parse().ok())
        .max()
        .unwrap_or(0)
}

/// Wait until the newest report was taken after everything sent so far.
///
/// A rejected gesture changes nothing, so reading the model straight after it
/// would also accept a report printed before the window handled the release.
/// One report can be in flight when this is called and the next can fall just
/// before the release is processed; once a third one has started, the second
/// is complete and later than both.
pub(crate) fn settle(h: &mut Harness) -> Result<()> {
    let before = last_tick(&h.text()?);
    h.wait(10, |h| Ok(last_tick(&h.text()?) > before + 2))
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
        .filter(|part| !part.is_empty())
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

fn tree_boxes(h: &Harness) -> Result<String> {
    let line = h.geometry()?;
    let start = line.find("tree_boxes=[").ok_or("tree boxes missing")?;
    let end = line[start..]
        .find(']')
        .map(|offset| start + offset)
        .ok_or("tree boxes unclosed")?;
    Ok(line[start..=end].to_string())
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

fn sidebar_colour(h: &Harness, index: usize) -> Result<usize> {
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
        .find_map(|entry| {
            let mut parts = entry.split(':');
            (parts.next() == Some(index.to_string().as_str()))
                .then(|| parts.next()?.parse().ok())
                .flatten()
        })
        .ok_or_else(|| format!("sidebar row {index} has no colour").into())
}

fn pane_colour(h: &Harness, pane: &str) -> Result<usize> {
    let text = h.text()?;
    let line = text
        .lines()
        .rfind(|line| line.contains("pane_colours=["))
        .ok_or("pane colours missing")?;
    let entries = line
        .split_once("pane_colours=[")
        .ok_or("pane colours missing")?
        .1
        .split(']')
        .next()
        .unwrap_or("");
    for entry in entries.split(',') {
        let entry = entry.trim_matches(['"', ' ', '[', ']']);
        if let Some((id, colour)) = entry.split_once(':') {
            if id == pane {
                return colour
                    .parse()
                    .map_err(|_| format!("pane {pane} colour is not a number").into());
            }
        }
    }
    Err(format!("pane {pane} has no colour").into())
}

fn pane_scroll(h: &Harness, pane: u64) -> Option<f64> {
    let text = h.text().ok()?;
    let line = text.lines().rfind(|line| line.contains("pane_scroll=["))?;
    let entries = line.split_once("pane_scroll=[")?.1.split(']').next()?;
    entries.split(',').find_map(|entry| {
        let entry = entry.trim_matches(['"', ' ', '[', ']']);
        let (id, rest) = entry.split_once(':')?;
        if id != pane.to_string() {
            return None;
        }
        rest.split('@').next()?.parse().ok()
    })
}

fn move_to(h: &Harness, rect: [f64; 4], fraction: f64) -> Result<()> {
    h.move_mouse(
        (rect[0] + rect[2] * 0.4) as i64,
        (rect[1] + rect[3] * fraction) as i64,
    )
}

fn pane_start(h: &Harness, pane: &str) -> Result<(i64, i64)> {
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

/// The header of the expanded pane.
///
/// `tree_boxes` reports the stored layout, where an expanded pane keeps its
/// place among the hidden ones, while on screen it fills the whole pane area.
/// Its header therefore starts at the top-left corner of the union of the
/// stored boxes.
fn expanded_start(h: &Harness) -> Result<(i64, i64)> {
    let geometry = h.geometry()?;
    let origin = crate::sessions::pair(&geometry, "origin", ',')?;
    let boxes = geometry
        .split_once("tree_boxes=[")
        .and_then(|(_, tail)| tail.split_once(']'))
        .map(|(boxes, _)| boxes)
        .ok_or("pane layout geometry missing")?;
    let corners: Vec<(f64, f64)> = boxes
        .split_whitespace()
        .filter_map(|entry| entry.split_once(':'))
        .filter_map(|(_, rect)| {
            let mut parts = rect.split(['+', 'x']);
            Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
        })
        .collect();
    let left = corners
        .iter()
        .map(|corner| corner.0)
        .fold(f64::MAX, f64::min);
    let top = corners
        .iter()
        .map(|corner| corner.1)
        .fold(f64::MAX, f64::min);
    require(!corners.is_empty(), "pane layout has no boxes")?;
    Ok((origin[0] + left as i64 + 120, origin[1] + top as i64 + 10))
}

fn begin_pane_drag(h: &mut Harness, pane: &str) -> Result<()> {
    let start = pane_start(h, pane)?;
    begin_drag_at(h, start)
}

fn begin_drag_at(h: &mut Harness, (x, y): (i64, i64)) -> Result<()> {
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

fn wait_no_pane_drag(h: &mut Harness) -> Result<()> {
    h.wait(10, |h| {
        let text = h.text()?;
        Ok(field(&text, "pane_drag").as_deref() == Some("none")
            && field(&text, "pane_indicators").as_deref() == Some("[]"))
    })
}

fn drop_pane_on(h: &mut Harness, pane: &str, destination: usize) -> Result<()> {
    begin_pane_drag(h, pane)?;
    hover_pane(h, destination)?;
    h.xdo(&["mouseup", "1"])?;
    wait_no_pane_drag(h)
}

fn wait_terminal_focus(h: &mut Harness, pane: &str) -> Result<()> {
    let focused = format!("\"{pane}:true:true\"");
    h.wait(10, |h| {
        Ok(h.text()?
            .lines()
            .rfind(|line| line.contains("pane_focus_states="))
            .is_some_and(|line| line.contains(&focused)))
    })?;
    // The self-check can report focus before the deferred rebuild has finished
    // reparenting the terminal. Give that turn a real completion window before
    // sending keys, or the first characters go to a widget being replaced.
    std::thread::sleep(Duration::from_millis(1200));
    Ok(())
}

fn set_directory(h: &mut Harness, pane: u64, directory: &Path) -> Result<()> {
    let path = directory.to_str().ok_or("test directory is not UTF-8")?;
    h.command(&format!(
        "cd '{}'; printf '\\033]7;file://localhost%s\\007' \"$PWD\"",
        path
    ))?;
    h.wait(10, |h| Ok(h.text()?.contains(&format!("{pane}:{path}"))))
}

fn write_file(h: &mut Harness, command: &str, path: &Path) -> Result<String> {
    // A restored session checks the same directory again under the same name;
    // an existing file would make the wait pass before the new command ran.
    let _ = fs::remove_file(path);
    h.command(&format!("{} > '{}'", command, path.display()))?;
    h.wait(15, |_| Ok(path.exists()))?;
    Ok(fs::read_to_string(path)?.trim().to_string())
}

fn assert_pwd(h: &mut Harness, directory: &Path) -> Result<()> {
    let output = h.base.join(format!(
        "pwd-{}",
        directory
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("check")
    ));
    let actual = write_file(h, "pwd", &output)?;
    require(
        actual == directory.to_str().ok_or("test directory is not UTF-8")?,
        "the moved shell is not in its original directory",
    )?;
    Ok(())
}

fn scroll_pane_with(h: &Harness, pane: &str, button: &str) -> Result<()> {
    let geometry = h.geometry()?;
    let origin = crate::sessions::pair(&geometry, "origin", ',')?;
    let rect = crate::sessions::pane_rectangles(&geometry, pane)
        .last()
        .copied()
        .ok_or("pane geometry missing")?;
    h.move_mouse(
        origin[0] + (rect[0] + rect[2] / 2.0) as i64,
        origin[1] + (rect[1] + rect[3] / 2.0) as i64,
    )?;
    h.xdo(&["click", "--repeat", "20", button])
}

fn scroll_pane(h: &Harness, pane: &str) -> Result<()> {
    scroll_pane_with(h, pane, "4")
}

pub fn run(binary: &Path, scratch: &Path) -> Result<PathBuf> {
    let mut h = fresh(binary, scratch, "among-pane-workspace-move")?;
    let base = h.base.clone();
    h.env.insert("AMONG_SELFCHECK_TICKS".into(), "2400".into());
    h.env.insert("AMONG_COLOR_SCHEME".into(), "light".into());
    h.start("pointer")?;
    h.ready()?;

    for count in 2..=3 {
        h.key("ctrl+shift+t")?;
        h.wait(10, |h| Ok(order(h)?.len() == count))?;
    }
    // The source workspace gets a second pane, so moving one out leaves a
    // workspace to return to before the later last-pane scenario.
    h.key("ctrl+shift+n")?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "panes").as_deref() == Some("2"))
    })?;

    let moved_dir = base.join("moved-source");
    let target_dir = base.join("target-project");
    let other_dir = base.join("other-project");
    for directory in [&moved_dir, &target_dir, &other_dir] {
        fs::create_dir(directory)?;
    }

    // The split focused pane 4. Give it a distinct directory, then do the same
    // for the first two workspaces so restoration has something to prove.
    set_directory(&mut h, 4, &moved_dir)?;
    h.key("alt+1")?;
    h.wait(10, |h| Ok(order(h)?.first().copied() == Some(1)))?;
    set_directory(&mut h, 1, &target_dir)?;
    h.key("alt+2")?;
    h.wait(10, |h| Ok(order(h)?.get(1).copied() == Some(2)))?;
    set_directory(&mut h, 2, &other_dir)?;
    h.key("alt+3")?;
    h.wait(10, |h| Ok(order(h)?.get(2).copied() == Some(3)))?;

    let before_order = order(&h)?;
    let before_counts = pane_counts(&h)?;
    let source_reference = field(&h.text()?, "active_reference")
        .ok_or("active reference missing")?
        .parse::<u64>()?;
    let shells_before = fs::read_to_string(base.join("shell-launches"))?;
    let pid_before = write_file(&mut h, "printf '%s' \"$$\"", &base.join("pane-pid-before"))?;

    // Build scrollback that must still be there after the terminal's container
    // is rebuilt in the destination.
    h.command("seq 1 500")?;
    std::thread::sleep(Duration::from_millis(700));

    begin_pane_drag(&mut h, "4")?;
    hover_pane(&mut h, 0)?;
    h.screenshot("pane-destination-light.png")?;
    let target_reference = order(&h)?.first().copied().ok_or("target missing")?;
    require(
        field(&h.text()?, "pane_target").as_deref() == Some(target_reference.to_string().as_str()),
        "the destination workspace was not reported",
    )?;
    h.xdo(&["mouseup", "1"])?;
    wait_no_pane_drag(&mut h)?;
    h.wait(10, |h| {
        let counts = pane_counts(h)?;
        Ok(order(h)? == before_order
            && counts.first().copied() == before_counts.first().map(|count| count + 1)
            && counts.last().copied() == before_counts.last().map(|count| count - 1)
            && field(&h.text()?, "active_reference").as_deref()
                == Some(target_reference.to_string().as_str())
            && field(&h.text()?, "focused").as_deref() == Some("PaneId(4)"))
    })?;
    wait_terminal_focus(&mut h, "4")?;

    require(
        fs::read_to_string(base.join("shell-launches"))? == shells_before,
        "moving a pane restarted a shell",
    )?;
    let pid_after = write_file(&mut h, "printf '%s' \"$$\"", &base.join("pane-pid-after"))?;
    require(
        pid_before == pid_after,
        "the moved pane has a different shell process",
    )?;
    assert_pwd(&mut h, &moved_dir)?;
    // The rebuild may return the terminal to its newest output, but the history
    // itself must still be there. Scroll up again and require a real position.
    scroll_pane(&h, "4")?;
    let mut previous = None;
    h.wait(10, |h| {
        let current = pane_scroll(h, 4);
        let stable = current.is_some_and(|lines| lines > 0.0) && current == previous;
        previous = current;
        Ok(stable)
    })?;
    require(
        pane_scroll(&h, 4).is_some_and(|lines| lines > 0.0),
        "moving the pane lost its scrollback history",
    )?;
    h.screenshot("pane-after-move.png")?;
    let target_colour = sidebar_colour(&h, 0)?;
    require(
        pane_colour(&h, "4")? == target_colour,
        "the moved pane's header does not use the destination colour",
    )?;
    println!("PASS a real pane drag moves the live shell, its directory, scrollback and header colour without reordering");

    // Returning to the source shows the sibling that was left behind focused.
    h.key("alt+3")?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "active_reference").as_deref()
            == Some(source_reference.to_string().as_str())
            && field(&h.text()?, "focused").as_deref() == Some("PaneId(3)"))
    })?;
    h.key("alt+1")?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "active_reference").as_deref()
            == Some(target_reference.to_string().as_str())
            && field(&h.text()?, "focused").as_deref() == Some("PaneId(4)"))
    })?;

    // Dropping on the pane's own workspace is a no-op. The pointer visits a
    // foreign entry first so the indication is known to be live; clearing it on
    // the own entry then proves that entry was actually under the pointer.
    begin_pane_drag(&mut h, "4")?;
    hover_pane(&mut h, 1)?;
    move_to(&h, row(&h, 0)?, 0.5)?;
    h.wait(10, |h| {
        let text = h.text()?;
        Ok(field(&text, "pane_target").as_deref() == Some("none")
            && field(&text, "pane_indicators").as_deref() == Some("[]"))
    })?;
    h.xdo(&["mouseup", "1"])?;
    wait_no_pane_drag(&mut h)?;
    settle(&mut h)?;
    require(
        order(&h)? == before_order
            && {
                let counts = pane_counts(&h)?;
                counts.first().copied() == before_counts.first().map(|count| count + 1)
                    && counts.last().copied() == before_counts.last().map(|count| count - 1)
            }
            && field(&h.text()?, "active_reference").as_deref()
                == Some(target_reference.to_string().as_str()),
        "a pane dropped on its own workspace changed the model",
    )?;

    // Dropping on the window chrome is outside both the sidebar and every pane,
    // so neither a workspace nor a pane target may accept it.
    begin_pane_drag(&mut h, "4")?;
    let before_layout = tree_boxes(&h)?;
    hover_pane(&mut h, 1)?;
    h.move_mouse(10, 10)?;
    h.wait(10, |h| {
        let text = h.text()?;
        Ok(field(&text, "pane_target").as_deref() == Some("none")
            && field(&text, "pane_indicators").as_deref() == Some("[]"))
    })?;
    h.xdo(&["mouseup", "1"])?;
    wait_no_pane_drag(&mut h)?;
    settle(&mut h)?;
    require(
        order(&h)? == before_order
            && tree_boxes(&h)? == before_layout
            && field(&h.text()?, "active_reference").as_deref()
                == Some(target_reference.to_string().as_str()),
        "a pane dropped on window chrome changed the workspace or layout",
    )?;

    // Empty sidebar space below the last entry names no workspace: the
    // indication from a foreign entry must clear and a release there must not
    // move anything.
    begin_pane_drag(&mut h, "4")?;
    hover_pane(&mut h, 1)?;
    let last = row(&h, before_order.len() - 1)?;
    move_to(
        &h,
        [last[0], last[1] + last[3] * 2.0, last[2], last[3]],
        0.5,
    )?;
    h.wait(10, |h| {
        let text = h.text()?;
        Ok(field(&text, "pane_target").as_deref() == Some("none")
            && field(&text, "pane_indicators").as_deref() == Some("[]"))
    })?;
    h.xdo(&["mouseup", "1"])?;
    wait_no_pane_drag(&mut h)?;
    settle(&mut h)?;
    require(
        order(&h)? == before_order
            && tree_boxes(&h)? == before_layout
            && field(&h.text()?, "active_reference").as_deref()
                == Some(target_reference.to_string().as_str()),
        "a pane dropped on empty sidebar space changed the workspace or layout",
    )?;

    // Escape cancels while the destination is previewed.
    begin_pane_drag(&mut h, "4")?;
    hover_pane(&mut h, 1)?;
    h.xdo(&["key", "Escape"])?;
    wait_no_pane_drag(&mut h)?;
    h.xdo(&["mouseup", "1"])?;
    // The release after Escape is the one that must not commit, so the model is
    // read from a report taken after the release was handled.
    settle(&mut h)?;
    require(
        order(&h)? == before_order
            && pane_counts(&h)? == {
                let mut counts = before_counts.clone();
                counts[0] += 1;
                counts[2] -= 1;
                counts
            }
            && field(&h.text()?, "active_reference").as_deref()
                == Some(target_reference.to_string().as_str()),
        "Escape still committed the pane move",
    )?;
    println!(
        "PASS source-entry, window-chrome, empty-sidebar and Escape pane gestures are rejected"
    );

    // Collapse the sidebar and move the remaining pane of the former source
    // workspace. That workspace loses its last pane and must disappear.
    h.button("toggle")?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "sidebar_collapsed").as_deref() == Some("true"))
    })?;
    h.key("alt+3")?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "active_reference").as_deref()
            == Some(source_reference.to_string().as_str()))
    })?;
    drop_pane_on(&mut h, "3", 1)?;
    h.wait(10, |h| {
        Ok(order(h)?.len() == 2
            && field(&h.text()?, "active_reference").as_deref()
                == order(h)?.get(1).map(ToString::to_string).as_deref()
            && field(&h.text()?, "focused").as_deref() == Some("PaneId(3)"))
    })?;
    let collapsed_counts = pane_counts(&h)?;
    require(
        collapsed_counts == [2, 2],
        "collapsed-strip move did not update both workspace counts",
    )?;
    h.screenshot("pane-destination-collapsed.png")?;
    h.button("toggle")?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "sidebar_collapsed").as_deref() == Some("false"))
    })?;
    require(
        pane_counts(&h)? == [2, 2] && order(&h)?.len() == 2,
        "expanding the sidebar lost the pane move",
    )?;
    println!(
        "PASS a collapsed-strip pane drop moves the last pane and closes the emptied workspace"
    );

    // Wait for the actual periodic checkpoint, then restore it.
    h.wait(15, |h| {
        Ok(h.snapshot_if_written().is_some_and(|snapshot| {
            snapshot["workspaces"].as_array().is_some_and(|workspaces| {
                workspaces.len() == 2
                    && workspaces.iter().any(|workspace| {
                        workspace["layout"].to_string().contains("\"id\":4")
                            && workspace["directories"]
                                .as_array()
                                .is_some_and(|directories| {
                                    directories.iter().any(|entry| {
                                        entry["pane"] == 4
                                            && entry["cwd"].as_str() == moved_dir.to_str()
                                    })
                                })
                    })
            })
        }))
    })?;
    let saved: Value = h
        .snapshot_if_written()
        .ok_or("checkpoint missing before restore")?;
    fs::write(
        base.join("saved-session.json"),
        serde_json::to_vec_pretty(&saved)?,
    )?;

    h.stop(true)?;
    h.start("restored")?;
    h.button("Restore")?;
    h.ready()?;
    let restored_order = order(&h)?;
    require(restored_order.len() == 2, "restoration lost a workspace")?;
    require(
        field(&h.text()?, "active_reference").as_deref()
            == restored_order.get(1).map(ToString::to_string).as_deref(),
        "the restored active workspace is wrong",
    )?;
    require(
        pane_counts(&h)? == [2, 2],
        "restored pane counts do not match the moved session",
    )?;
    h.key("alt+1")?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "active_reference").as_deref()
            == restored_order.first().map(ToString::to_string).as_deref())
    })?;
    wait_terminal_focus(&mut h, "4")?;
    assert_pwd(&mut h, &moved_dir)?;
    println!("PASS a saved session restores the moved pane in its destination with its directory");

    edge_phases(binary, scratch)?;
    Ok(base)
}

/// Expanded panes on either side of a move, and a source whose process exits
/// while its header is being dragged. Run in a fresh window so the main
/// scenario's counts and restored session stay exactly as they were.
fn edge_phases(binary: &Path, scratch: &Path) -> Result<()> {
    let mut h = fresh(binary, scratch, "among-pane-workspace-move-edges")?;
    h.env.insert("AMONG_SELFCHECK_TICKS".into(), "2400".into());
    h.start("edges")?;
    h.ready()?;
    // Workspace one: panes 1 and 2. Workspace two: panes 3 and 4.
    h.key("ctrl+shift+n")?;
    h.wait(10, |h| Ok(pane_counts(h)? == [2]))?;
    h.key("ctrl+shift+t")?;
    h.wait(10, |h| Ok(order(h)?.len() == 2))?;
    h.key("ctrl+shift+n")?;
    h.wait(10, |h| Ok(pane_counts(h)? == [2, 2]))?;
    let references = order(&h)?;

    // An expanded destination: the drop must end the expansion and show the
    // new split, with focus on the pane that arrived.
    h.key("alt+1")?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "active_reference") == Some(references[0].to_string()))
    })?;
    h.key("ctrl+shift+x")?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "visible").as_deref() == Some("1"))
    })?;
    h.key("alt+2")?;
    h.wait(10, |h| {
        Ok(
            field(&h.text()?, "active_reference") == Some(references[1].to_string())
                && field(&h.text()?, "visible").as_deref() == Some("2"),
        )
    })?;
    drop_pane_on(&mut h, "4", 0)?;
    h.wait(10, |h| {
        let text = h.text()?;
        Ok(pane_counts(h)? == [3, 1]
            && field(&text, "active_reference") == Some(references[0].to_string())
            && field(&text, "visible").as_deref() == Some("3")
            && field(&text, "focused").as_deref() == Some("PaneId(4)"))
    })?;
    wait_terminal_focus(&mut h, "4")?;
    println!(
        "PASS a drop into an expanded workspace ends its expansion and focuses the moved pane"
    );

    // An expanded source: only the expanded pane's header is on screen, and
    // dragging it away must leave the source's normal layout behind.
    h.key("ctrl+shift+x")?;
    h.wait(10, |h| {
        Ok(field(&h.text()?, "visible").as_deref() == Some("1")
            && field(&h.text()?, "focused").as_deref() == Some("PaneId(4)"))
    })?;
    let start = expanded_start(&h)?;
    begin_drag_at(&mut h, start)?;
    hover_pane(&mut h, 1)?;
    h.xdo(&["mouseup", "1"])?;
    wait_no_pane_drag(&mut h)?;
    h.wait(10, |h| {
        let text = h.text()?;
        Ok(pane_counts(h)? == [2, 2]
            && field(&text, "active_reference") == Some(references[1].to_string())
            && field(&text, "visible").as_deref() == Some("2")
            && field(&text, "focused").as_deref() == Some("PaneId(4)"))
    })?;
    h.key("alt+1")?;
    h.wait(10, |h| {
        Ok(
            field(&h.text()?, "active_reference") == Some(references[0].to_string())
                && field(&h.text()?, "visible").as_deref() == Some("2"),
        )
    })?;
    println!("PASS moving an expanded pane out restores the source's normal layout");

    // The dragged pane's shell exits while the destination is previewed. The
    // gesture must not move anything: the pane closes in its own workspace and
    // the destination keeps its panes.
    let focused = field(&h.text()?, "focused").ok_or("focused pane missing")?;
    let pane = focused
        .strip_prefix("PaneId(")
        .and_then(|rest| rest.strip_suffix(')'))
        .ok_or("focused pane has an unexpected form")?
        .to_string();
    let pid_file = h.base.join("exiting-pid");
    let pid = write_file(&mut h, "printf '%s' \"$$\"", &pid_file)?;
    begin_pane_drag(&mut h, &pane)?;
    hover_pane(&mut h, 1)?;
    let killed = std::process::Command::new("kill")
        .args(["-KILL", pid.trim()])
        .status()?;
    require(
        killed.success(),
        "the dragged pane's shell could not be stopped",
    )?;
    h.wait(10, |h| Ok(pane_counts(h)?.first().copied() == Some(1)))?;
    h.xdo(&["mouseup", "1"])?;
    wait_no_pane_drag(&mut h)?;
    settle(&mut h)?;
    require(
        order(&h)? == references
            && pane_counts(&h)? == [1, 2]
            && field(&h.text()?, "active_reference") == Some(references[0].to_string()),
        "a pane whose process exited during the drag was moved",
    )?;
    println!("PASS a pane whose process exits during the drag is not moved");
    h.clean_log(&h.log)?;
    h.stop(false)?;
    Ok(())
}
