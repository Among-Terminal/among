use crate::{
    sessions::{fresh, pair, pane_rectangles, rectangles, Harness},
    storage, Result,
};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn require(value: bool, message: &str) -> Result<()> {
    if value {
        Ok(())
    } else {
        Err(message.into())
    }
}
#[track_caller]
fn wait(h: &mut Harness, offset: usize, seconds: u64, test: impl Fn(&str) -> bool) -> Result<()> {
    let caller = std::panic::Location::caller();
    let result = h.wait(seconds, |this| {
        Ok(this.text()?.get(offset..).is_some_and(&test))
    });
    if let Err(error) = result {
        let _ = h.screenshot("status-timeout.png");
        if let Ok(processes) = h.probe("/usr/bin/ps", &["-eo", "pid,ppid,args"]) {
            let _ = fs::write(h.base.join("timeout-processes.txt"), processes);
        }
        return Err(format!("live status at {caller}: {error}").into());
    }
    Ok(())
}

fn footer_directory(text: &str, pane: u64, directory: &str) -> bool {
    // A footer showing a directory below /tmp, such as the scratch root the
    // check starts in, is not evidence that `cd /tmp` actually reached the shell.
    text.replace("\\\"", "\"")
        .contains(&format!("{pane}:[\"{directory}|"))
}

#[cfg(test)]
mod directory_tests {
    use super::*;
    #[test]
    fn a_shared_directory_prefix_is_not_shell_command_evidence() {
        let scratch = crate::SCRATCH_ROOT;
        assert!(!footer_directory(
            &format!("rendered_footers=[\"4:[\\\"{scratch}/test|{scratch}/test|10x10\\\"]\"]"),
            4,
            "/tmp"
        ));
        assert!(footer_directory(
            "rendered_footers=[\"4:[\\\"/tmp|/tmp|10x10\\\"]\"]",
            4,
            "/tmp"
        ));
        assert!(!footer_directory(
            "rendered_footers=[\"4:[\\\"/var/tmp|/var/tmp|10x10\\\"]\"]",
            4,
            "/var"
        ));
    }
}
fn quoted_values<'a>(text: &'a str, prefix: &str) -> Vec<&'a str> {
    text.match_indices(prefix)
        .filter_map(|(index, _)| text[index + prefix.len()..].split('"').next())
        .collect()
}
fn clock(value: &str) -> bool {
    value.len() == 8
        && value.bytes().enumerate().all(|(i, b)| {
            if i == 2 || i == 5 {
                b == b':'
            } else {
                b.is_ascii_digit()
            }
        })
}

fn footer_widths(text: &str) -> Vec<u64> {
    text.match_indices("2:[\\\"/")
        .filter_map(|(index, _)| {
            text[index..]
                .split("\\\"")
                .nth(1)
                .and_then(|value| value.rsplit_once('|'))
                .and_then(|(_, tail)| tail.split_once('x'))
                .and_then(|(width, _)| width.parse::<u64>().ok())
        })
        .collect()
}

pub fn run(root: &Path, binary: &Path, plugins: &Path, scratch: &Path) -> Result<PathBuf> {
    let mut h = fresh(binary, scratch, "live-status")?;
    let names = ["clock", "system-status", "pane-pwd"];
    let plugin_dir = h.base.join("data/among/plugins");
    fs::create_dir(&plugin_dir)?;
    for name in names {
        let bytes = fs::read(storage::safe_path(plugins, Path::new(name))?)?;
        require(
            bytes.starts_with(b"\x7fELF"),
            "status plugin is not native ELF",
        )?;
        storage::atomic_write(&plugin_dir, Path::new(name), &bytes, 0o755)?;
    }
    fs::write(
        h.base.join("config/among/plugins.json"),
        serde_json::to_vec(
            &json!({"plugins":names.map(|name|json!({"name":name,"enabled":true}))}),
        )?,
    )?;
    let docs = fs::read_to_string(root.join("docs/plugins.md"))?;
    let hook = docs
        .split_once("among_osc7() {")
        .and_then(|(_, tail)| tail.split_once("```"))
        .ok_or("documented OSC7 hook missing")?
        .0;
    storage::atomic_write(&h.base,Path::new("shell"),format!("#!/bin/bash\namong_osc7() {{{hook}\nexport -f among_osc7\nexport PROMPT_COMMAND\nexec /bin/bash --noprofile --norc\n").as_bytes(),0o755)?;
    fs::write(h.base.join("steps"), b"split\nsplit\nnew-workspace\n")?;
    h.env
        .insert("AMONG_SCRIPT".into(), h.base.join("steps").into_os_string());
    let pid = h.start("app")?;
    wait(&mut h, 0, 15, |text| {
        text.contains("sidebar_states=[0:0:false:3 1:1:false:1]")
            && text.contains("rendered_footers=[\"")
            && text.contains("DISK ")
            && text.contains("rendered_meters=[\"")
            && names
                .iter()
                .all(|name| text.contains(&format!("{name}=Running")))
    })?;
    // Startup has no dialog to dismiss. An unsolicited Escape reaches Bash
    // Readline, leaving a Meta prefix that consumes the next typed character.
    wait(&mut h, 0, 15, |text| {
        text.contains("4:true:true") && text.contains("50x4")
    })?;
    require(
        h.probe("/usr/bin/xdotool", &["getwindowname", &h.window])?
            .trim()
            == "Among Terminal",
        "window identity changed",
    )?;
    require(
        h.probe("/usr/bin/xprop", &["-id", &h.window, "_NET_WM_STATE"])?
            .contains("_NET_WM_STATE_MAXIMIZED_VERT"),
        "window not maximized",
    )?;
    let offset = h.text()?.len();
    h.key("F11")?;
    wait(&mut h, offset, 15, |text| text.contains("fullscreen=true"))?;
    require(
        h.probe("/usr/bin/xprop", &["-id", &h.window, "_NET_WM_STATE"])?
            .contains("_NET_WM_STATE_FULLSCREEN"),
        "native fullscreen state missing",
    )?;
    let offset = h.text()?.len();
    h.key("F11")?;
    wait(&mut h, offset, 15, |text| text.contains("fullscreen=false"))?;
    h.wait(15, |this| {
        Ok(!this
            .probe("/usr/bin/xprop", &["-id", &this.window, "_NET_WM_STATE"])?
            .contains("_NET_WM_STATE_FULLSCREEN"))
    })?;
    let offset = h.text()?.len();
    h.command("cd /tmp")?;
    wait(&mut h, offset, 15, |text| footer_directory(text, 4, "/tmp"))?;
    h.command("sleep 1; cd /var")?;
    h.key("ctrl+Page_Up")?;
    std::thread::sleep(Duration::from_secs(2));
    h.key("ctrl+Page_Down")?;
    wait(&mut h, 0, 15, |text| footer_directory(text, 4, "/var"))?;
    h.screenshot("workspace-green.png")?;
    h.key("ctrl+Page_Up")?;
    let offset = h.text()?.len();
    h.key("ctrl+shift+w")?;
    wait(&mut h, offset, 15, |text| text.contains("2:true:true"))?;
    h.key("ctrl+shift+n")?;
    wait(&mut h, 0, 15, |text| text.contains("5:true:true"))?;
    let offset = h.text()?.len();
    h.command("exit")?;
    wait(&mut h, offset, 15, |text| text.contains("2:true:true"))?;
    let offset = h.text()?.len();
    h.command("cd /var")?;
    let base = h.base.to_string_lossy().to_string();
    wait(&mut h, offset, 2, |text| {
        footer_directory(text, 2, "/var")
            && text.contains(&base)
            && text.contains("rendered_footers=")
    })?;
    h.screenshot("status.png")?;
    let geometry = h.geometry()?;
    let origin = pair(&geometry, "origin", ',')?;
    let source = pane_rectangles(&geometry, "2")
        .last()
        .copied()
        .ok_or("pane2 bounds missing")?;
    let target = pane_rectangles(&geometry, "1")
        .last()
        .copied()
        .ok_or("pane1 bounds missing")?;
    let sx = origin[0] + (source[0] + source[2] / 2.0) as i64;
    let sy = origin[1] + source[1] as i64 + 12;
    h.move_mouse(sx, sy)?;
    h.xdo(&["mousedown", "1"])?;
    std::thread::sleep(Duration::from_millis(200));
    let offset = h.text()?.len();
    for (x, y) in [
        (sx - 30, sy),
        (
            origin[0] + (target[0] + target[2] / 2.0) as i64,
            origin[1] + (target[1] + target[3] / 2.0) as i64,
        ),
        (
            origin[0] + target[0] as i64 + 8,
            origin[1] + (target[1] + target[3] / 2.0) as i64,
        ),
    ] {
        h.move_mouse(x, y)?;
        std::thread::sleep(Duration::from_millis(300));
    }
    h.xdo(&["mouseup", "1"])?;
    wait(&mut h, offset, 15, |text| text.contains("tree_boxes=[2:0+"))?;
    let geometry = h.geometry()?;
    require(
        pane_rectangles(&geometry, "2")
            .last()
            .ok_or("pane2 missing")?[0]
            < pane_rectangles(&geometry, "1")
                .last()
                .ok_or("pane1 missing")?[0],
        "pane drag did not move left",
    )?;
    wait(&mut h, offset, 15, |text| {
        text.replace("\\\"", "\"").contains("2:[\"/var") && text.contains(&base)
    })?;
    let long_path = h
        .base
        .join(format!("project-{}", "名前".repeat(25)))
        .join(format!("src-{}", "名前".repeat(25)));
    fs::create_dir_all(&long_path)?;
    let path = long_path.to_str().ok_or("long cwd is not UTF-8")?;
    let escaped = path
        .bytes()
        .map(|byte| format!("\\x{byte:02x}"))
        .collect::<String>();
    h.command(&format!("cd $'{escaped}'"))?;
    wait(&mut h, 0, 15, |text| {
        text.contains(path) && text.contains("rendered_footers=")
    })?;
    h.key("alt+F10")?;
    h.xdo(&["windowsize", "--sync", &h.window, "900", "600"])?;
    let offset = h.text()?.len();
    wait(&mut h, offset, 15, |text| {
        text.contains(path)
            && text.contains("rendered_footers=")
            && text.lines().any(|line| {
                line.split_once("window_width=")
                    .and_then(|(_, tail)| tail.split_whitespace().next())
                    .and_then(|value| value.parse::<u64>().ok())
                    .is_some_and(|width| width > 0 && width <= 900)
            })
    })?;
    let text = h.text()?;
    let narrow = text.get(offset..).ok_or("status log shrank")?;
    let sizes = footer_widths(narrow);
    require(
        !sizes.is_empty() && sizes.iter().all(|width| *width < 500),
        "long footer is unbounded",
    )?;
    h.screenshot("narrow.png")?;
    let resource = h.probe(
        "/usr/bin/pgrep",
        &["-P", &pid.to_string(), "-x", "system-status"],
    )?;
    let resource = resource
        .split_whitespace()
        .next()
        .ok_or("resource process missing")?
        .parse::<i32>()?;
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(resource),
        nix::sys::signal::Signal::SIGKILL,
    )?;
    let offset = h.text()?.len();
    wait(&mut h, offset, 15, |text| {
        text.contains("system-status=Faulted")
    })?;
    h.command("printf \"\\033]0;alive-after-resource-kill\\007\"")?;
    wait(&mut h, offset, 15, |text| {
        text.contains("alive-after-resource-kill")
    })?;
    let text = h.text()?;
    require(
        text.contains("pane_numbers=[\"#1\", \"#2\", \"#3\"]"),
        "built-in pane numbers missing",
    )?;
    let clocks = quoted_values(&text, "app_footer=[\"")
        .into_iter()
        .filter(|value| clock(value))
        .collect::<BTreeSet<_>>();
    require(clocks.len() >= 2, "clock did not tick")?;
    let mut aligned = false;
    for line in text.lines() {
        if let Some((_, tail)) = line.split_once("footer_rects=[\"") {
            if let Some((name, _)) = tail.split_once(':') {
                let name = tail.get(..8).filter(|value| clock(value)).unwrap_or(name);
                let rects = rectangles(tail, name);
                if let (Some(bounds), Some(width)) = (
                    rects.first(),
                    line.split_once("window_width=")
                        .and_then(|(_, tail)| tail.split_whitespace().next())
                        .and_then(|value| value.parse::<f64>().ok()),
                ) {
                    aligned |= (bounds[0] + bounds[2] - width).abs() <= 12.0;
                }
            }
        }
    }
    require(aligned, "clock footer not right aligned")?;
    require(
        quoted_values(&text, "footer_fonts=[\"")
            .iter()
            .any(|value| {
                value
                    .split_once('|')
                    .is_some_and(|(time, font)| clock(time) && font.starts_with("monospace Bold"))
            }),
        "clock font changed",
    )?;
    let opacities = quoted_values(&text, "footer_fonts=[\"")
        .into_iter()
        .filter_map(|value| {
            value
                .split_once("|opacity=")
                .and_then(|(_, tail)| tail.parse::<f64>().ok())
        })
        .collect::<Vec<_>>();
    require(
        opacities.len() >= 3
            && opacities
                .iter()
                .rev()
                .take(3)
                .all(|value| (*value - 0.7).abs() < 0.01),
        "clock opacity changed",
    )?;
    require(
        !quoted_values(&text, "app_header=[\"")
            .iter()
            .any(|value| clock(value)),
        "clock leaked into header",
    )?;
    require(
        text.match_indices(":50x4")
            .filter(|(index, _)| {
                text[..*index].rsplit_once("0.").is_some_and(|(_, tail)| {
                    !tail.is_empty() && tail.bytes().all(|byte| byte.is_ascii_digit())
                })
            })
            .count()
            >= 3,
        "resource bar geometry missing",
    )?;
    h.clean_log(&h.log)?;
    h.stop(false)?;
    println!("PASS three plugins, built-in counts, footer clock, bars, own/background PWD, close focus and fault containment\nEvidence: {}",h.base.display());
    Ok(h.base.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escaped_unicode_tooltips_do_not_hide_measured_footer_width() {
        let text = r#"rendered_footers=["2:[\"/project-名前…|/project-名前/src-名前|320x22\"]", "1:[\"/home|/home|319x22\"]"]"#;
        assert_eq!(footer_widths(text), vec![320]);
        assert!(footer_widths("no measured footer").is_empty());
        assert_eq!(
            quoted_values(
                "app_footer=[\"12:30:45\"] app_footer=[\"12:30:46\"]",
                "app_footer=[\""
            ),
            vec!["12:30:45", "12:30:46"]
        );
        assert!(clock("12:30:45"));
        assert!(!clock("12:30"));
    }
}
