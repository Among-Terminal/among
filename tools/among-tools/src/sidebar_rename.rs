//! Check renaming a workspace from its sidebar entry.
//!
//! A single click switches workspaces and nothing else, a double-click opens a
//! field holding the name, and the rename ends the same way whichever way it
//! ends. The check drives real clicks at the bounds the application publishes
//! and reads the application's own report back, so a gesture that does not fire
//! is visible rather than assumed.
//!
//! It also takes a fingerprint of the workspace before and after a rename, so
//! "a rename changes nothing else" is established rather than asserted, and it
//! types into the terminal without clicking afterwards, which is the only way to
//! show that the terminal — not a field that no longer exists — has the keyboard.

use crate::{
    sessions::{fresh, pair, rectangles, Harness},
    Result,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The fields of a report line, as `name=value` pairs.
///
/// A value may contain spaces — a directory may — so a value runs to the next
/// field's name rather than to the next space.
fn fields(line: &str) -> Vec<(String, String)> {
    let is_name = |candidate: &str| {
        !candidate.is_empty()
            && candidate
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_lowercase())
            && candidate
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    // A field's name starts at the character after a space, so those positions
    // are where a name may begin.
    let starts: Vec<usize> = (0..line.len())
        .filter(|at| *at == 0 || line.as_bytes()[*at - 1] == b' ')
        .filter(|at| {
            line[*at..]
                .split_once('=')
                .is_some_and(|(name, _)| is_name(name))
        })
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(position, at)| {
            let name_start = *at;
            let name_end = line[name_start..]
                .find('=')
                .map(|at| name_start + at)
                .unwrap_or(name_start);
            let value_start = name_end + 1;
            let value_end = starts.get(position + 1).copied().unwrap_or(line.len());
            (
                line[name_start..name_end].to_string(),
                line[value_start..value_end].trim_end().to_string(),
            )
        })
        .collect()
}

/// The value of one field in the most recent report.
fn field(h: &Harness, key: &str) -> Result<String> {
    let report = h.text()?;
    let report = report
        .lines()
        .rev()
        .find(|line| line.contains(&format!(" {key}=")))
        .ok_or_else(|| missing_field(key))?;
    fields(report)
        .into_iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
        .ok_or_else(|| missing_field(key))
}

/// A named field the report did not carry.
fn missing_field(key: &str) -> Box<dyn std::error::Error + Send + Sync> {
    format!("the report has no `{key}` field").into()
}

/// Everything the report says about what a workspace contains.
///
/// Compared across a rename: the panes, their directories, their colours and
/// titles, the layout, and what plugins have contributed to the sidebar.
fn contents(h: &Harness) -> Result<String> {
    const KEYS: [&str; 7] = [
        "workspaces",
        "panes",
        "tree_boxes",
        "pane_titles",
        "pane_colours",
        "pane_cwds",
        "workspace_summaries",
    ];
    let mut parts = Vec::new();
    for key in KEYS {
        parts.push(format!("{key}={}", field(h, key)?));
    }
    Ok(parts.join(" "))
}

/// Whether the self-check last reported row `index` as being edited.
fn is_editing(h: &Harness, index: u64) -> Option<bool> {
    let report = h.text().ok()?;
    let states = report
        .lines()
        .rev()
        .find(|line| line.contains(" sidebar_states=["))?;
    let list = states
        .split_once(" sidebar_states=[")?
        .1
        .split(']')
        .next()?;
    list.split_whitespace().find_map(|entry| {
        let mut parts = entry.split(':');
        let row = parts.next()?.parse::<u64>().ok()?;
        let _colour = parts.next()?;
        let editing = parts.next()?;
        (row == index).then(|| editing == "true")
    })
}

/// The workspace names the report gives.
fn reported_names(h: &Harness) -> Vec<String> {
    field(h, "names")
        .map(|names| {
            names
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split('|')
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The workspace index the report gives as active.
fn active(h: &Harness) -> Option<usize> {
    let report = h.text().ok()?;
    let line = report
        .lines()
        .rev()
        .find(|line| line.contains(" active="))?;
    let active = line.split_once(" active=")?.1;
    active.split_whitespace().next()?.parse().ok()
}

/// The workspace names in the session file the application wrote.
fn snapshot_names(h: &Harness) -> Option<Vec<String>> {
    let snapshot = h.snapshot_if_written()?;
    let workspaces = snapshot["workspaces"].as_array()?;
    Some(
        workspaces
            .iter()
            .map(|workspace| workspace["name"].as_str().unwrap_or_default().to_string())
            .collect(),
    )
}

/// The bounds the application publishes for a workspace's name.
fn name_rect(h: &Harness, index: u64) -> Result<[f64; 4]> {
    let geometry = h.geometry()?;
    let origin = pair(&geometry, "sidebar_origin", ',')?;
    let bounds = rectangles(
        geometry
            .split_once("name_rects=[")
            .map(|(_, tail)| tail)
            .ok_or("name rectangles missing")?,
        &index.to_string(),
    )
    .first()
    .copied()
    .ok_or("workspace name rect missing")?;
    Ok([
        origin[0] as f64 + bounds[0],
        origin[1] as f64 + bounds[1],
        bounds[2],
        bounds[3],
    ])
}

/// Move the pointer to a point and confirm it arrived there.
///
/// A click aimed with a stale or misread rectangle silently lands somewhere
/// else, which reads as an application that ignores clicks. The pointer's real
/// position is therefore part of what this check establishes.
fn move_to(h: &Harness, x: i64, y: i64) -> Result<()> {
    h.move_mouse(x, y)?;
    let reported = h.probe("/usr/bin/xdotool", &["getmouselocation", "--shell"])?;
    let read = |key: &str| {
        reported.lines().find_map(|line| {
            line.strip_prefix(&format!("{key}="))?
                .trim()
                .parse::<i64>()
                .ok()
        })
    };
    let (got_x, got_y) = (read("X"), read("Y"));
    if got_x != Some(x) || got_y != Some(y) {
        return Err(
            format!("the pointer was asked for ({x},{y}) and is at ({got_x:?},{got_y:?})").into(),
        );
    }
    Ok(())
}

/// Click a published name rectangle, once or twice.
fn click_name(h: &Harness, index: u64, clicks: usize) -> Result<()> {
    let bounds = name_rect(h, index)?;
    move_to(
        h,
        (bounds[0] + bounds[2] / 2.0).round() as i64,
        (bounds[1] + bounds[3] / 2.0).round() as i64,
    )?;
    if clicks == 1 {
        return h.xdo(&["click", "1"]);
    }
    h.xdo(&[
        "click",
        "--repeat",
        &clicks.to_string(),
        "--delay",
        "80",
        "1",
    ])
}

/// Open the name field on a row and wait for it to report itself.
fn open_field(h: &mut Harness, index: u64) -> Result<()> {
    // Long enough for the previous click sequence to end. Two double-clicks closer
    // together than GTK's threshold arrive as one sequence of four presses, which is
    // not the two separate double-clicks this check means to send.
    std::thread::sleep(Duration::from_millis(450));
    click_name(h, index, 2)?;
    h.wait(15, |this| Ok(is_editing(this, index) == Some(true)))
}

/// Replace whatever the field holds with `typed` and leave it open.
fn retype(h: &Harness, typed: &str) -> Result<()> {
    h.key("ctrl+a")?;
    h.xdo(&["type", "--clearmodifiers", typed])
}

fn require(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

pub fn run(binary: &Path, scratch: &Path) -> Result<PathBuf> {
    let mut h = fresh(binary, scratch, "among-rename")?;
    h.env.insert("AMONG_SELFCHECK_TICKS".into(), "900".into());
    h.start("rename")?;
    h.wait(15, |this| Ok(this.text()?.contains("has_focus=true")))?;

    // A second workspace, so switching and leaving have somewhere to go.
    h.key("ctrl+shift+t")?;
    h.wait(15, |this| Ok(this.text()?.contains("workspaces=2")))?;

    // Clicks work at all: a click in the pane area moves that pane's focus. If
    // this fails, nothing below it means anything.
    let geometry = h.geometry()?;
    let origin = pair(&geometry, "origin", ',')?;
    let pane = crate::sessions::pane_rectangles(&geometry, "2")
        .first()
        .copied()
        .ok_or("the second pane has no geometry")?;
    move_to(
        &h,
        origin[0] + (pane[0] + pane[2] / 2.0) as i64,
        origin[1] + pane[1] as i64 + 60,
    )?;
    let offset = h.text()?.len();
    h.xdo(&["click", "1"])?;
    h.wait(15, |this| {
        Ok(this
            .text()?
            .get(offset..)
            .is_some_and(|text| text.contains("focused=PaneId(2)")))
    })?;
    println!("PASS a click in the pane area focuses that pane");

    let first = {
        let names = reported_names(&h);
        require(
            names.len() == 2,
            &format!(
                "two workspaces are needed, the report gives {}",
                field(&h, "names")?
            ),
        )?;
        names[0].clone()
    };

    // A single click switches and opens nothing.
    let offset = h.text()?.len();
    click_name(&h, 0, 1)?;
    h.wait(15, |this| {
        Ok(this
            .text()?
            .get(offset..)
            .is_some_and(|text| text.contains("active=0 ")))
    })?;
    require(
        is_editing(&h, 0) == Some(false),
        "a single click must not open the name field",
    )?;
    println!("PASS a single click switches to the workspace and opens no field");

    // The field opens where the name was, so a pointer finds the same place in
    // either state.
    let label_rect = name_rect(&h, 0)?;
    open_field(&mut h, 0)?;
    let entry_rect = name_rect(&h, 0)?;
    let within = |a: f64, b: f64| (a - b).abs() <= 2.0;
    require(
        within(entry_rect[0], label_rect[0]) && within(entry_rect[1], label_rect[1]),
        &format!("the field opened at {entry_rect:?} where the name was at {label_rect:?}"),
    )?;
    println!("PASS the field opens in the place the name was");

    // The field holds the name, and the keyboard is in it: deleting the last
    // character and confirming leaves exactly the name minus that character.
    let mut shortened = first.clone();
    shortened.pop();
    h.key("End")?;
    h.key("BackSpace")?;
    h.key("Return")?;
    h.wait(15, |this| {
        Ok(this.text()?.contains(&format!("[{shortened}|")) && is_editing(this, 0) == Some(false))
    })?;
    println!("PASS the field holds the current name and takes the keyboard");

    // Naming it and confirming applies the new name, and reaches the session.
    open_field(&mut h, 0)?;
    retype(&h, "renamed by check")?;
    let changed = Instant::now();
    h.key("Return")?;
    h.wait(15, |this| {
        Ok(this.text()?.contains("renamed by check") && is_editing(this, 0) == Some(false))
    })?;
    require(
        snapshot_names(&h).is_some_and(|names| names.contains(&"renamed by check".to_string())),
        "the applied rename did not reach the session file",
    )?;
    require(
        changed.elapsed() < Duration::from_secs(3),
        "workspace name checkpoint exceeded bound",
    )?;
    println!("PASS Enter applies the typed name and removes the field");

    // A cancelled rename keeps the old one.
    open_field(&mut h, 0)?;
    retype(&h, "discarded")?;
    h.key("Escape")?;
    h.wait(15, |this| {
        Ok(this.text()?.contains("renamed by check") && is_editing(this, 0) == Some(false))
    })?;
    println!("PASS Escape keeps the previous name");

    // A name that cannot be used is refused however the rename ends, and the row
    // shows the name the workspace still holds once the field is gone.
    for (ending, blank) in [
        ("Return", ""),
        ("Return", "   "),
        ("click", ""),
        ("click", "   "),
    ] {
        open_field(&mut h, 0)?;
        retype(&h, blank)?;
        if ending == "Return" {
            h.key("Return")?;
        } else {
            click_name(&h, 1, 1)?;
        }
        h.wait(15, |this| Ok(is_editing(this, 0) == Some(false)))?;
        let names = reported_names(&h);
        require(
            names.first() == Some(&"renamed by check".to_string()),
            &format!("a blank name ({blank:?}) ended with {ending} was applied: {names:?}"),
        )?;
        click_name(&h, 0, 1)?;
        h.wait(15, |this| Ok(active(this) == Some(0)))?;
    }
    println!("PASS a blank name is refused on every ending, and the row shows the old name");

    // Moving away applies what was typed.
    open_field(&mut h, 0)?;
    retype(&h, "applied by leaving")?;
    click_name(&h, 1, 1)?;
    h.wait(15, |this| {
        Ok(this.text()?.contains("applied by leaving") && is_editing(this, 0) == Some(false))
    })?;
    println!("PASS moving away applies the typed name");

    // A rename changes nothing else in its workspace: the same workspace, active and
    // focused on both sides of it, has to report the same contents. Compared here
    // rather than across a phase that switches workspaces, which changes the pane the
    // report describes without a rename being involved.
    click_name(&h, 0, 1)?;
    h.wait(15, |this| Ok(active(this) == Some(0)))?;
    let before = contents(&h)?;
    open_field(&mut h, 0)?;
    retype(&h, "renamed in place")?;
    h.key("Return")?;
    h.wait(15, |this| {
        Ok(this.text()?.contains("renamed in place") && is_editing(this, 0) == Some(false))
    })?;
    require(
        contents(&h)? == before,
        &format!(
            "a rename changed the workspace: {before} became {}",
            contents(&h)?
        ),
    )?;
    println!("PASS a rename changes nothing else in the workspace");

    // The terminal has the keyboard again: this is typed without clicking, so it
    // only lands if the terminal — not a field that is gone — has focus.
    let proof = h.base.join("rename-focus-proof.txt");
    h.xdo(&[
        "type",
        "--clearmodifiers",
        &format!("echo proof > {}", proof.display()),
    ])?;
    h.key("Return")?;
    h.wait(15, |_| Ok(proof.exists()))?;
    println!("PASS the terminal keeps the keyboard after a rename");

    // Starting a rename on another row ends the first one, leaves exactly one
    // field, and leaves the active marker on the workspace being renamed.
    open_field(&mut h, 1)?;
    retype(&h, "second workspace")?;
    open_field(&mut h, 0)?;
    h.wait(15, |this| {
        Ok(is_editing(this, 1) == Some(false) && is_editing(this, 0) == Some(true))
    })?;
    let names = reported_names(&h);
    require(
        names.first() == Some(&"renamed in place".to_string())
            && names.get(1) == Some(&"second workspace".to_string()),
        &format!("the earlier rename did not end on the earlier name: {names:?}"),
    )?;
    require(
        active(&h) == Some(0),
        "the workspace being renamed must be the active one",
    )?;
    h.key("Escape")?;
    h.wait(15, |this| Ok(is_editing(this, 0) == Some(false)))?;
    println!("PASS only one workspace is renamed at a time, and it is the active one");

    // What the sidebar shows is what the session file holds.
    let names = reported_names(&h);
    require(
        snapshot_names(&h).as_deref() == Some(names.as_slice()),
        &format!("the session file does not carry the sidebar's names: {names:?}"),
    )?;
    println!("PASS the sidebar and the session file agree on every name");

    // The collapsed sidebar has no names to show and no room for a name field, and a
    // single click still has to switch workspaces there. Run in a second launch,
    // because the sidebar is collapsed from a startup script step rather than from
    // a click — and a click on the collapse control would itself be the pointer work
    // this phase is meant to leave alone.
    let script = h.base.join("collapsed.script");
    std::fs::write(&script, b"collapse-sidebar\n")?;
    h.env.insert("AMONG_SCRIPT".into(), script.into_os_string());
    // The session the first launch wrote is removed rather than restored: a launch
    // that finds one asks whether to restore it, and the application stays behind
    // that question — with no window focus, no keyboard and no sidebar to click.
    h.forget_session()?;
    h.start("collapsed")?;
    h.wait(15, |this| Ok(this.text()?.contains("has_focus=true")))?;
    // The second workspace is created with the keyboard rather than in the script: a
    // script step runs before the session is ready, and creating a workspace is
    // refused until it is, so a scripted one is dropped.
    h.key("ctrl+shift+t")?;
    h.wait(20, |this| {
        Ok(
            this.text()?.contains("workspaces=2")
                && this.text()?.contains("sidebar_collapsed=true"),
        )
    })?;
    require(
        field(&h, "name_rects")?.trim_start_matches('[') == "]",
        "a collapsed sidebar must show no names",
    )?;
    let geometry = h.geometry()?;
    let origin = pair(&geometry, "sidebar_origin", ',')?;
    let strip = rectangles(
        geometry
            .split_once("sidebar_rows=[")
            .map(|(_, tail)| tail)
            .ok_or("sidebar rows missing")?,
        "1",
    )
    .first()
    .copied()
    .ok_or("the collapsed strip has no rows")?;
    move_to(
        &h,
        origin[0] + (strip[0] + strip[2] / 2.0) as i64,
        origin[1] + (strip[1] + strip[3] / 2.0) as i64,
    )?;
    let offset = h.text()?.len();
    h.xdo(&["click", "1"])?;
    h.wait(15, |this| {
        Ok(this
            .text()?
            .get(offset..)
            .is_some_and(|text| text.contains("active=1 ")))
    })?;
    require(
        field(&h, "sidebar_states")?.contains(":false:"),
        "a single click in a collapsed sidebar must not open a name field",
    )?;
    println!("PASS the collapsed sidebar switches on a single click and shows no names");

    h.clean_log(&h.log)?;
    h.stop(false)?;
    println!("Evidence: {}", h.base.display());
    Ok(h.base.clone())
}

#[cfg(test)]
mod tests {
    use super::fields;

    const REPORT: &str = concat!(
        "among[tick3]: workspaces=2 active=1 name=\"project 2\" visible=1 panes=1 ",
        "paned_widgets=1 focused=PaneId(2) area=1590x1144 origin=320,46 ",
        "tree_boxes=[2:0+0+1590x1144] has_focus=true terminal_sizes=[198x65] exited=[] ",
        "sidebar_collapsed=false fullscreen=false title_buttons=(true, true) windows=1 ",
        "sidebar_origin=0,46 sidebar_rows=[0:0+0+320x40 1:0+40+320x40] ",
        "sidebar_states=[0:0:false:1 1:1:false:1] ",
        "name_rects=[0:17+6+45x28 1:17+46+68x28] names=[project|project 2] rows_in_list=2"
    );

    fn value(key: &str) -> Option<String> {
        fields(REPORT)
            .into_iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    #[test]
    fn a_field_runs_to_the_next_field() {
        assert_eq!(value("panes").as_deref(), Some("1"));
        assert_eq!(value("workspaces").as_deref(), Some("2"));
        assert_eq!(value("focused").as_deref(), Some("PaneId(2)"));
    }

    #[test]
    fn a_value_may_hold_spaces_and_brackets() {
        assert_eq!(value("names").as_deref(), Some("[project|project 2]"));
        assert_eq!(
            value("name_rects").as_deref(),
            Some("[0:17+6+45x28 1:17+46+68x28]")
        );
        assert_eq!(value("tree_boxes").as_deref(), Some("[2:0+0+1590x1144]"));
    }

    #[test]
    fn the_last_field_runs_to_the_end_of_the_line() {
        assert_eq!(value("rows_in_list").as_deref(), Some("2"));
    }
}
