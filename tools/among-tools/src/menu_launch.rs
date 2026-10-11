use crate::{
    menu_geometry::settled_row,
    sessions::{fresh, pair, pane_rectangles as rectangles, Harness},
    storage, Result,
};
use serde_json::json;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn record(output: &Path, arguments: &[String]) -> Result<()> {
    let base = output.parent().ok_or("URI receipt has no parent")?;
    if !base
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("among-menu-uri-"))
        || output.file_name() != Some(std::ffi::OsStr::new("received.jsonl"))
    {
        return Err("URI receipt is not an owned menu fixture".into());
    }
    storage::safe_path(base, Path::new("received.jsonl"))?;
    let _lock = storage::ExclusiveLock::acquire(base, Path::new("receipt.lock"))?;
    let mut file = OpenOptions::new().append(true).create(true).open(output)?;
    file.write_all(&serde_json::to_vec(arguments)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn latest(text: &str) -> &str {
    text.rfind("]: header_icons=")
        .and_then(|position| text[..position].rfind("among[").map(|start| &text[start..]))
        .unwrap_or(text)
}
fn wait(h: &mut Harness, test: impl Fn(&str) -> bool) -> Result<()> {
    h.wait(15, |this| Ok(test(latest(&this.text()?))))
}
fn menu(h: &mut Harness, uri: &str) -> Result<usize> {
    h.command(&format!("printf '\\033[2J\\033[H{uri}\\n'"))?;
    std::thread::sleep(Duration::from_millis(500));
    point(h)?;
    let offset = h.text()?.len();
    h.xdo(&["click", "3"])?;
    wait(h, |text| {
        text.contains("terminal_menus=[1]") && text.contains("Open Link:")
    })?;
    Ok(offset)
}
fn point(h: &Harness) -> Result<()> {
    let geometry = h.geometry()?;
    let origin = pair(&geometry, "origin", ',')?;
    let bounds = rectangles(&geometry, "1")
        .last()
        .copied()
        .ok_or("URI pane geometry missing")?;
    h.move_mouse(
        origin[0] + bounds[0] as i64 + 36,
        origin[1] + bounds[1] as i64 + 38,
    )
}
fn open(h: &mut Harness, offset: usize) -> Result<()> {
    h.wait(15, |this| {
        Ok(settled_row(
            this.text()?.get(offset..).ok_or("URI log shrank")?,
            1,
            "Open Link",
        )?
        .is_some())
    })?;
    let bounds = settled_row(
        h.text()?.get(offset..).ok_or("URI log shrank")?,
        1,
        "Open Link",
    )?
    .ok_or("settled Open Link missing")?;
    h.move_mouse(bounds[0] + bounds[2] / 2, bounds[1] + bounds[3] / 2)?;
    h.xdo(&["click", "1"])?;
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}

pub fn run(binary: &Path, scratch: &Path) -> Result<PathBuf> {
    let mut supported = fresh(binary, scratch, "among-menu-uri")?;
    let base = supported.base.clone();
    let received = base.join("received.jsonl");
    let tools = std::env::current_exe()?;
    let tools = tools.to_str().ok_or("URI handler path is not UTF-8")?;
    let output = received.to_str().ok_or("URI output path is not UTF-8")?;
    // our generated paths are controlled; desktop field codes still require escaping.
    let quote = |value: &str| -> Result<String> {
        if value.contains(['"', '\\', '\n', '\r', '`', '$']) {
            return Err("unexpected URI fixture path characters".into());
        }
        Ok(format!("\"{}\"", value.replace('%', "%%")))
    };
    let applications = base.join("data/applications");
    fs::create_dir(&applications)?;
    fs::write(applications.join("among-private-uri.desktop"),format!("[Desktop Entry]\nType=Application\nName=Private Among URI checker\nExec={} record-menu-uri {} %u\nTerminal=false\nMimeType=x-scheme-handler/http;x-scheme-handler/https;x-scheme-handler/mailto;\n",quote(tools)?,quote(output)?))?;
    fs::write(
        base.join("config/mimeapps.list"),
        format!(
            "[Default Applications]\n{}",
            ["http", "https", "mailto"]
                .map(|scheme| format!("x-scheme-handler/{scheme}=among-private-uri.desktop\n"))
                .join("")
        ),
    )?;
    let mut results = Vec::new();
    for phase in ["supported", "failure"] {
        let mut h = if phase == "supported" {
            supported
        } else {
            fresh(binary, scratch, "among-menu-uri-failure")?
        };
        h.env
            .insert("XDG_DATA_DIRS".into(), h.base.join("data").into_os_string());
        h.env.insert(
            "GSETTINGS_SCHEMA_DIR".into(),
            "/usr/share/glib-2.0/schemas".into(),
        );
        h.start("app")?;
        wait(&mut h, |text| text.contains("1:true:true"))?;
        if phase == "supported" {
            let uris = [
                "https://example.test/path?a=1&b=2",
                "http://example.test/owned",
                "mailto:demo@example.test",
            ];
            for (index, uri) in uris.iter().enumerate() {
                let scheme = uri.split_once(':').ok_or("test URI has no scheme")?.0;
                if !h
                    .probe(
                        "/usr/bin/gio",
                        &["mime", &format!("x-scheme-handler/{scheme}")],
                    )?
                    .contains("among-private-uri.desktop")
                {
                    return Err("private URI handler association missing".into());
                }
                let offset = menu(&mut h, uri)?;
                open(&mut h, offset)?;
                h.wait(10, |_| {
                    Ok(received.exists() && fs::read_to_string(&received)?.lines().count() > index)
                })?;
                let rows = fs::read_to_string(&received)?
                    .lines()
                    .map(|line| storage::parse_json(line.as_bytes()))
                    .collect::<Result<Vec<_>>>()?;
                if rows
                    != uris[..=index]
                        .iter()
                        .map(|value| json!([value]))
                        .collect::<Vec<_>>()
                {
                    return Err("private handler arguments differ from exact requested URI".into());
                }
                wait(&mut h, |text| {
                    text.contains("1:true:true") && text.contains("terminal_menus=[]")
                })?;
            }
            h.command("printf '\\033[2J\\033[H\\033]8;;javascript:synthetic-only\\033\\owned-link\\033]8;;\\033\\\\n'")?;
            std::thread::sleep(Duration::from_millis(500));
            point(&h)?;
            h.xdo(&["click", "3"])?;
            wait(&mut h, |text| {
                text.contains("terminal_menus=[1]") && text.contains("Open Link:false:")
            })?;
            h.key("Escape")?;
            results.push(
                "http/https/mailto exact private handler arguments; unsupported OSC8 cannot launch",
            );
        } else {
            let offset = menu(&mut h, "https://example.test/no-handler")?;
            open(&mut h, offset)?;
            h.wait(10, |this| {
                Ok(this.text()?.contains("among: Could not open link:"))
            })?;
            h.probe(
                "/usr/bin/import",
                &[
                    "-window",
                    "root",
                    h.base
                        .join("failure-toast.png")
                        .to_str()
                        .ok_or("screenshot path is not UTF-8")?,
                ],
            )?;
            h.command("printf '\\033]0;uri-failure-survivor\\007'")?;
            wait(&mut h, |text| {
                text.contains("1|uri-failure-survivor|") && text.contains("1:true:true")
            })?;
            fs::write(
                base.join("failure-evidence.txt"),
                h.base.to_string_lossy().as_bytes(),
            )?;
            results.push("missing private handler reports failure; terminal accepts input without host fallback");
        }
        if fs::read_to_string(&received)?.lines().count() != 3 {
            return Err("unsupported/failed URI invocation reached a handler".into());
        }
        h.clean_log(&h.log)?;
        h.stop(false)?;
        println!("PASS {}", results.last().ok_or("URI result missing")?);
        // retain ownership between loop iterations without leaking a running process.
        supported = h;
    }
    storage::atomic_write(
        &base,
        Path::new("results.json"),
        &serde_json::to_vec_pretty(&json!({"passed":results}))?,
        0o600,
    )?;
    println!("Evidence: {}", base.display());
    Ok(base)
}
