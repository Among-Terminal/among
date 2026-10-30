//! Real pointer resizing, fresh-process preference restoration and quiet notices.
use crate::sessions::{fresh, rectangles, Harness};
use crate::{storage, Result};
use serde_json::{json, Value};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn require(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

fn field(h: &Harness, name: &str) -> Result<String> {
    let text = h.text()?;
    let needle = format!(" {name}=");
    text.lines()
        .rev()
        .find_map(|line| {
            line.split_once(&needle)?
                .1
                .split_whitespace()
                .next()
                .map(str::to_string)
        })
        .ok_or_else(|| format!("missing window field {name}").into())
}

fn width(h: &Harness) -> Result<f64> {
    Ok(field(h, "sidebar_width")?.parse()?)
}

fn rect(h: &Harness, name: &str) -> Result<[f64; 4]> {
    rectangles(&h.text()?, name)
        .last()
        .copied()
        .filter(|r| r[2] > 0.0 && r[3] > 0.0)
        .ok_or_else(|| format!("missing allocated {name} rectangle").into())
}

fn drag(h: &Harness, delta: i64) -> Result<()> {
    let r = rect(h, "edge")?;
    let x = (r[0] + r[2] / 2.0).round() as i64;
    let y = (r[1] + r[3] / 2.0).round() as i64;
    h.move_mouse(x, y)?;
    h.xdo(&["mousedown", "1"])?;
    let moved = (|| {
        for step in 1..=8 {
            h.move_mouse((x + delta * step / 8).max(0), y)?;
            std::thread::sleep(Duration::from_millis(30));
        }
        Ok(())
    })();
    h.xdo(&["mouseup", "1"])?;
    moved
}

fn toggle(h: &Harness) -> Result<()> {
    let r = rect(h, "toggle")?;
    h.move_mouse((r[0] + r[2] / 2.0) as i64, (r[1] + r[3] / 2.0) as i64)?;
    h.xdo(&["click", "1"])
}

fn restart(h: &mut Harness, phase: &str) -> Result<()> {
    h.clean_log(&h.log)?;
    h.stop(false)?;
    h.forget_session()?;
    h.start(phase)?;
    h.ready()
}

fn configuration(h: &Harness) -> PathBuf {
    h.base.join("config/among/window.json")
}
fn saved(h: &Harness) -> Result<Value> {
    storage::parse_json(&fs::read(configuration(h))?)
}

pub fn run(binary: &Path, notifier: &Path, scratch: &Path) -> Result<PathBuf> {
    let mut h = fresh(binary, scratch, "among-window-controls")?;
    let program = crate::live_status::profile::bounded(notifier, 8 * 1024 * 1024)?;
    crate::managed_fixture::install(
        &h.base,
        "notifier",
        &program,
        &["status", "context.terminals", "events.terminal"],
    )?;
    h.start("drag")?;
    h.ready()?;
    h.wait(15, |this| {
        Ok(this
            .text()?
            .contains("github:12/notifier:message type is not recognised")
            && this.text()?.contains("app_footer=[\"fixture-status\"]")
            && field(this, "toast_widgets")? == "0"
            && field(this, "notices_recorded")? == "0"
            && !this.text()?.contains("notification-check-must-not-escape"))
    })?;
    require(
        !configuration(&h).exists(),
        "startup wrote an unselected width",
    )?;
    require(
        field(&h, "sidebar_preferred")? == "-",
        "missing preference did not use defaults",
    )?;
    require(
        field(&h, "sidebar_resize_cursor")? == "col-resize",
        "resize cursor missing",
    )?;
    println!("PASS synthetic managed fixture: unsupported notify is a value-free refusal, explicit footer is visible, no notice or toast is fabricated");

    h.key("ctrl+shift+n")?;
    h.wait(15, |this| {
        Ok(this.geometry()?.contains("panes=2")
            && this.text()?.contains("fixture-status-pane-opened")
            && field(this, "notices_recorded")? == "0"
            && field(this, "toast_widgets")? == "0")
    })?;
    h.command("printf '%s' \"$$\" > before-resize.pid")?;
    h.wait(10, |this| Ok(this.base.join("before-resize.pid").exists()))?;
    let layout =
        h.snapshot_if_written().ok_or("missing session")?["workspaces"][0]["layout"].clone();
    let before = width(&h)?;
    drag(&h, 140)?;
    h.wait(10, |this| {
        Ok(width(this)? > before + 100.0 && configuration(this).exists())
    })?;
    let selected = saved(&h)?["sidebar_width"]
        .as_f64()
        .ok_or("width not saved")?;
    require(
        (width(&h)? - selected).abs() <= 4.0,
        "saved width does not control the sidebar",
    )?;
    h.wait(10, |this| {
        Ok(field(this, "toast_widgets")? == "0" && this.geometry()?.contains("has_focus=true"))
    })?;
    h.xdo(&[
        "type",
        "--clearmodifiers",
        "printf '%s' \"$$\" > after-resize.pid",
    ])?;
    h.key("Return")?;
    h.wait(10, |this| Ok(this.base.join("after-resize.pid").exists()))?;
    require(
        fs::read(h.base.join("before-resize.pid"))? == fs::read(h.base.join("after-resize.pid"))?,
        "resize restarted the shell",
    )?;
    require(
        h.snapshot_if_written().ok_or("missing resized session")?["workspaces"][0]["layout"]
            == layout,
        "resize changed the pane layout",
    )?;
    println!(
        "PASS real drag changes and saves width; pane layout, shell and keyboard focus survive"
    );

    toggle(&h)?;
    h.wait(10, |this| {
        Ok(field(this, "sidebar_resize_visible")? == "false" && width(this)? < 80.0)
    })?;
    let collapsed = width(&h)?;
    h.move_mouse(collapsed as i64 - 2, 150)?;
    h.xdo(&["mousedown", "1"])?;
    h.move_mouse(collapsed as i64 + 98, 150)?;
    h.xdo(&["mouseup", "1"])?;
    std::thread::sleep(Duration::from_millis(200));
    require(
        (width(&h)? - collapsed).abs() < 1.0 && saved(&h)?["sidebar_width"] == json!(selected),
        "collapsed strip accepted a resize",
    )?;
    toggle(&h)?;
    h.wait(10, |this| {
        Ok(field(this, "sidebar_resize_visible")? == "true"
            && (width(this)? - selected).abs() <= 4.0)
    })?;
    println!("PASS collapsed strip is fixed and expansion restores the selected width");

    h.key("ctrl+shift+x")?;
    h.key("ctrl+shift+x")?;
    h.wait(10, |this| {
        Ok(field(this, "toast_widgets")? == "0" && this.geometry()?.contains("panes=2"))
    })?;
    h.command("exit 7")?;
    h.wait(10, |this| {
        Ok(this.geometry()?.contains("panes=1") && this.text()?.contains("exited with status"))
    })?;
    h.clean_log(&h.log)?;
    println!("PASS zoom, restore and process exit create no toast widgets");

    restart(&mut h, "fresh-process")?;
    h.wait(10, |this| Ok((width(this)? - selected).abs() <= 4.0))?;
    let bytes = fs::read(configuration(&h))?;
    h.key("alt+F10")?;
    h.xdo(&["windowsize", "--sync", &h.window, "600", "650"])?;
    h.wait(10, |this| Ok(width(this)? < selected - 20.0))?;
    require(
        fs::read(configuration(&h))? == bytes,
        "window shrink overwrote the desired width",
    )?;
    h.key("alt+F10")?;
    h.wait(10, |this| Ok((width(this)? - selected).abs() <= 4.0))?;
    println!("PASS fresh process restores width independently of session restoration; small windows only clamp its allocation");

    let config = h.base.join("config/among");
    fs::set_permissions(&config, fs::Permissions::from_mode(0o500))?;
    let failure = (|| -> Result<()> {
        drag(&h, 50)?;
        h.wait(10, |this| {
            Ok(this.text()?.contains("settings save failed")
                && this.text()?.contains("session-only"))
        })?;
        require(
            fs::read(configuration(&h))? == bytes,
            "failed save replaced the previous preference",
        )?;
        require(
            width(&h)? > selected + 20.0,
            "failed save discarded the live width",
        )
    })();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o700))?;
    failure?;
    drag(&h, -20)?;
    h.wait(10, |this| {
        Ok(saved(this)?["sidebar_width"]
            .as_f64()
            .is_some_and(|width| width != selected))
    })?;
    println!("PASS save failure keeps live width and old bytes, reports diagnostics without a toast and recovers");

    for (phase, body) in [
        ("malformed", "broken"),
        ("future", "{\"version\":2}"),
        ("invalid", "{\"version\":1,\"sidebar_width\":0}"),
    ] {
        h.stop(false)?;
        h.forget_session()?;
        fs::write(configuration(&h), body)?;
        h.start(phase)?;
        h.ready()?;
        require(
            field(&h, "sidebar_preferred")? == "-",
            "invalid preference changed width",
        )?;
        let before = width(&h)?;
        drag(&h, 50)?;
        h.wait(10, |this| {
            Ok(this.text()?.contains("settings save failed") && width(this)? >= before + 48.0)
        })?;
        require(
            fs::read_to_string(configuration(&h))? == body,
            "invalid preferences were overwritten",
        )?;
        h.clean_log(&h.log)?;
    }
    println!("PASS malformed, future-version and invalid-width preferences do not block startup or get overwritten");
    let maximum = field(&h, "window_width")?.parse::<f64>()? - 240.0;
    drag(&h, 5000)?;
    h.wait(10, |this| {
        Ok(width(this)? >= maximum - 4.0 && width(this)? <= maximum)
    })?;
    drag(&h, -5000)?;
    h.wait(10, |this| Ok((width(this)? - 180.0).abs() <= 2.0))?;
    println!("PASS edge drags stop at both usable width limits");
    h.screenshot("window-controls.png")?;
    h.stop(false)?;
    println!("Evidence: {}", h.base.display());
    Ok(h.base.clone())
}
