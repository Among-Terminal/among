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

/// Write a private desktop handler into this harness's own XDG data home.
///
/// Returns the file where the handler records the arguments it was invoked
/// with, so a check can prove exactly which target was opened. Each harness
/// has its own data home, so no personal or system association is touched.
fn handler(h: &Harness) -> Result<PathBuf> {
    let tools = std::env::current_exe()?;
    let tools = tools.to_str().ok_or("URI handler path is not UTF-8")?;
    let received = h.base.join("received.jsonl");
    let output = received.to_str().ok_or("URI output path is not UTF-8")?;
    // our generated paths are controlled; desktop field codes still require escaping.
    let quote = |value: &str| -> Result<String> {
        if value.contains(['"', '\\', '\n', '\r', '`', '$']) {
            return Err("unexpected URI fixture path characters".into());
        }
        Ok(format!("\"{}\"", value.replace('%', "%%")))
    };
    let applications = h.base.join("data/applications");
    fs::create_dir_all(&applications)?;
    fs::write(applications.join("among-private-uri.desktop"),format!("[Desktop Entry]\nType=Application\nName=Private Among URI checker\nExec={} record-menu-uri {} %u\nTerminal=false\nMimeType=x-scheme-handler/http;x-scheme-handler/https;x-scheme-handler/mailto;\n",quote(tools)?,quote(output)?))?;
    fs::write(
        h.base.join("config/mimeapps.list"),
        format!(
            "[Default Applications]\n{}",
            ["http", "https", "mailto"]
                .map(|scheme| format!("x-scheme-handler/{scheme}=among-private-uri.desktop\n"))
                .join("")
        ),
    )?;
    Ok(received)
}

/// Point a harness at its own handler set and the host's schema data.
fn prepare(h: &mut Harness) {
    h.env
        .insert("XDG_DATA_DIRS".into(), h.base.join("data").into_os_string());
    h.env.insert(
        "GSETTINGS_SCHEMA_DIR".into(),
        "/usr/share/glib-2.0/schemas".into(),
    );
}

fn receipts(path: &Path) -> Result<Vec<Vec<String>>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    fs::read_to_string(path)?
        .lines()
        .map(|line| {
            Ok(serde_json::from_value(storage::parse_json(
                line.as_bytes(),
            )?)?)
        })
        .collect()
}

/// Require exactly these handler invocations, in this order.
fn expect(path: &Path, expected: &[&str]) -> Result<()> {
    let actual = receipts(path)?;
    let expected: Vec<Vec<String>> = expected.iter().map(|uri| vec![uri.to_string()]).collect();
    if actual != expected {
        return Err(format!("private handler receipts differ: {actual:?}").into());
    }
    Ok(())
}

/// Wait until the handler has recorded `count` invocations, and no more.
fn wait_receipts(h: &mut Harness, path: &Path, count: usize) -> Result<()> {
    h.wait(15, |_| Ok(receipts(path)?.len() >= count))?;
    let actual = receipts(path)?.len();
    if actual != count {
        return Err(format!("expected {count} handler invocations, saw {actual}").into());
    }
    Ok(())
}

/// The newest live report line carrying `name`.
///
/// The report is printed as several lines, so each field is read from the line
/// that carries it rather than from one assumed shape.
fn line(h: &Harness, name: &str) -> Result<String> {
    let text = h.text()?;
    text.lines()
        .rev()
        .find(|line| line.contains(name))
        .map(str::to_string)
        .ok_or_else(|| format!("no live report line for {name}").into())
}

/// The items of a bracketed list field in the newest report.
fn list(h: &Harness, name: &str) -> Result<String> {
    let line = line(h, &format!("{name}=["))?;
    let (_, tail) = line
        .split_once(&format!("{name}=["))
        .ok_or_else(|| format!("state report has no {name}"))?;
    let (items, _) = tail
        .split_once(']')
        .ok_or_else(|| format!("state report {name} is not a list"))?;
    Ok(items.to_string())
}

/// Whether the newest report shows no open context menu anywhere.
fn no_menu(h: &Harness) -> Result<bool> {
    Ok(list(h, "terminal_menus")?.trim().is_empty())
}

/// The panes the newest report shows with a selection.
fn selections(h: &Harness) -> Result<String> {
    Ok(list(h, "terminal_selections")?.replace(' ', ""))
}

/// The focused pane the newest report shows.
fn focused(h: &Harness) -> Result<String> {
    let line = line(h, " focused=")?;
    let (_, value) = line
        .split_once(" focused=")
        .ok_or("state report has no focused pane")?;
    Ok(value.split(' ').next().unwrap_or_default().to_string())
}

/// Whether the newest report shows this pane focused: its terminal holding the
/// keyboard focus and its frame carrying the focused marker.
fn pane_focused(h: &Harness, pane: u64) -> Result<bool> {
    let expected = format!("{pane}:true:true");
    Ok(list(h, "pane_focus_states")?
        .split(',')
        .map(|entry| entry.trim().trim_matches('"'))
        .any(|entry| entry == expected))
}

/// One pane's entry in a `pane:id=value@rowheight` list.
fn pane_entry(h: &Harness, pane: u64) -> Result<String> {
    for entry in list(h, "pane_scroll")?.split(',') {
        let entry = entry.trim().trim_matches('"');
        let Some((id, rest)) = entry.split_once(':') else {
            continue;
        };
        if id.parse::<u64>().ok() == Some(pane) {
            return Ok(rest.to_string());
        }
    }
    Err("pane scroll report missing".into())
}

/// The scroll distance the newest report shows for `pane`.
fn scroll_of(h: &Harness, pane: u64) -> Result<f64> {
    Ok(pane_entry(h, pane)?
        .split_once('@')
        .map(|(lines, _)| lines)
        .unwrap_or_default()
        .parse()?)
}

/// The reported terminal row height for `pane`.
///
/// Read from the live report rather than assumed, so a click on a later row
/// follows whatever font the check runs under.
fn row_height(h: &Harness, pane: u64) -> Result<i64> {
    let entry = pane_entry(h, pane)?;
    let height = entry
        .split_once('@')
        .map(|(_, height)| height)
        .unwrap_or_default();
    let height: i64 = height.parse()?;
    if height <= 0 {
        return Err("pane row height is not positive".into());
    }
    Ok(height)
}

/// The centre of a text cell in `pane`, in window coordinates.
///
/// The first row's vertical offset is the one the menu phases already aim at;
/// later rows are one reported row height apart.
fn cell(h: &Harness, pane: u64, row: i64) -> Result<(i64, i64)> {
    let geometry = h.geometry()?;
    let origin = pair(&geometry, "origin", ',')?;
    let bounds = rectangles(&geometry, &pane.to_string())
        .last()
        .copied()
        .ok_or("pointer pane geometry missing")?;
    let height = row_height(h, pane)?;
    Ok((
        origin[0] + bounds[0] as i64 + 36,
        origin[1] + bounds[1] as i64 + 38 + row * height,
    ))
}

fn click_at(h: &Harness, at: (i64, i64)) -> Result<()> {
    h.move_mouse(at.0, at.1)?;
    std::thread::sleep(Duration::from_millis(200));
    h.xdo(&["click", "1"])?;
    std::thread::sleep(Duration::from_millis(400));
    Ok(())
}

fn ctrl_click_at(h: &Harness, at: (i64, i64)) -> Result<()> {
    h.move_mouse(at.0, at.1)?;
    std::thread::sleep(Duration::from_millis(200));
    h.xdo(&["keydown", "Control_L"])?;
    let clicked = h.xdo(&["click", "1"]);
    let released = h.xdo(&["keyup", "Control_L"]);
    clicked?;
    released?;
    std::thread::sleep(Duration::from_millis(400));
    Ok(())
}

/// Press on `from`, drag to `to` and release, optionally holding Ctrl.
fn drag(h: &Harness, from: (i64, i64), to: (i64, i64), ctrl: bool) -> Result<()> {
    h.move_mouse(from.0, from.1)?;
    std::thread::sleep(Duration::from_millis(200));
    if ctrl {
        h.xdo(&["keydown", "Control_L"])?;
    }
    let mut result = h.xdo(&["mousedown", "1"]);
    if result.is_ok() {
        std::thread::sleep(Duration::from_millis(150));
        result = h.move_mouse(to.0, to.1);
    }
    if result.is_ok() {
        std::thread::sleep(Duration::from_millis(150));
        result = h.xdo(&["mouseup", "1"]);
    }
    if ctrl {
        let keys = h.xdo(&["keyup", "Control_L"]);
        if result.is_ok() {
            result = keys;
        }
    }
    result?;
    std::thread::sleep(Duration::from_millis(400));
    Ok(())
}

/// Clear the focused pane's screen and print one line, as the shell would.
fn print(h: &Harness, line: &str) -> Result<()> {
    h.command(&format!("printf '\\033[2J\\033[H{line}\\n'"))?;
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}

/// Print one line through an explicit OSC 8 hyperlink.
fn print_link(h: &Harness, target: &str, text: &str) -> Result<()> {
    h.command(&format!(
        "printf '\\033[2J\\033[H\\033]8;;{target}\\033\\\\{text}\\033]8;;\\033\\\\\\n'"
    ))?;
    std::thread::sleep(Duration::from_millis(500));
    Ok(())
}

fn contents(h: &Harness, name: &str) -> Result<Vec<u8>> {
    Ok(fs::read(h.base.join(name))?)
}

/// Record and announce one passed case, as the menu phases do.
fn pass(results: &mut Vec<String>, text: &str) {
    println!("PASS {text}");
    results.push(text.into());
}

/// The pointer cases: what Ctrl+click opens, and what it must leave alone.
fn click_phases(binary: &Path, scratch: &Path, results: &mut Vec<String>) -> Result<()> {
    let mut h = fresh(binary, scratch, "among-menu-uri-clicks")?;
    let received = handler(&h)?;
    prepare(&mut h);
    // Two panes, so a link can be fed into one and clicked while another holds
    // the focus that the click is supposed to move.
    let script = h.base.join("steps");
    fs::write(&script, b"split-pane\n")?;
    h.env.insert("AMONG_SCRIPT".into(), script.into_os_string());
    h.start("clicks")?;
    h.wait(15, |this| {
        let text = this.text()?;
        Ok(text.contains("panes=2") && text.contains("2:true:true"))
    })?;

    // A plain absolute URL opens, once, and leaves the menu closed.
    print(&h, "https://example.test/click-plain")?;
    ctrl_click_at(&h, cell(&h, 2, 0)?)?;
    wait_receipts(&mut h, &received, 1)?;
    expect(&received, &["https://example.test/click-plain"])?;
    h.wait(15, |this| no_menu(this))?;
    h.wait(15, |this| Ok(selections(this)?.is_empty()))?;

    // An explicit hyperlink whose display text is not its target opens the
    // target, not the label.
    print_link(&h, "https://example.test/osc8-target", "display text")?;
    ctrl_click_at(&h, cell(&h, 2, 0)?)?;
    wait_receipts(&mut h, &received, 2)?;
    expect(
        &received,
        &[
            "https://example.test/click-plain",
            "https://example.test/osc8-target",
        ],
    )?;
    pass(
        results,
        "ctrl+click opens a plain URL and an OSC8 target instead of its display text",
    );

    // Sentence punctuation is not part of a plain match, and a plain click on
    // the same link opens nothing at all.
    print(&h, "https://example.test/punct.")?;
    ctrl_click_at(&h, cell(&h, 2, 0)?)?;
    wait_receipts(&mut h, &received, 3)?;
    click_at(&h, cell(&h, 2, 0)?)?;
    std::thread::sleep(Duration::from_millis(600));
    expect(
        &received,
        &[
            "https://example.test/click-plain",
            "https://example.test/osc8-target",
            "https://example.test/punct",
        ],
    )?;
    h.wait(15, |this| no_menu(this))?;
    pass(
        results,
        "trailing sentence punctuation is dropped; a plain click on a link opens nothing",
    );

    // Ordinary text and a hyperlink whose scheme must not launch are left to
    // the terminal, and a later link click still works.
    print(&h, "ordinary text")?;
    ctrl_click_at(&h, cell(&h, 2, 0)?)?;
    std::thread::sleep(Duration::from_millis(600));
    print_link(&h, "custom:synthetic-only", "custom-link")?;
    ctrl_click_at(&h, cell(&h, 2, 0)?)?;
    std::thread::sleep(Duration::from_millis(600));
    expect(
        &received,
        &[
            "https://example.test/click-plain",
            "https://example.test/osc8-target",
            "https://example.test/punct",
        ],
    )?;
    h.wait(15, |this| no_menu(this))?;
    print(&h, "https://example.test/after-denial")?;
    ctrl_click_at(&h, cell(&h, 2, 0)?)?;
    wait_receipts(&mut h, &received, 4)?;
    pass(
        results,
        "ordinary text and a custom: hyperlink launch nothing and do not wedge later clicks",
    );

    // An openable link opens without disturbing a selection made elsewhere in
    // the same terminal, and without moving its history.
    print(&h, "https://example.test/selection")?;
    h.command("printf 'selectable words\\n'")?;
    std::thread::sleep(Duration::from_millis(500));
    let from = cell(&h, 2, 1)?;
    drag(&h, from, (from.0 + 260, from.1), false)?;
    h.wait(15, |this| Ok(selections(this)? == "2"))?;
    let selected = selections(&h)?;
    let scrolled = scroll_of(&h, 2)?;
    ctrl_click_at(&h, cell(&h, 2, 0)?)?;
    wait_receipts(&mut h, &received, 5)?;
    if selections(&h)? != selected {
        return Err("ctrl+click changed the terminal selection".into());
    }
    if (scroll_of(&h, 2)? - scrolled).abs() > f64::EPSILON {
        return Err("ctrl+click moved the terminal history".into());
    }
    pass(
        results,
        "ctrl+click opens a link without changing an existing selection or scroll position",
    );

    // A drag that starts on a link with Ctrl held opens the link and does not
    // become a selection. The previous case's selection is cleared first, so an
    // empty result afterwards is the drag's own doing.
    print(&h, "https://example.test/drag")?;
    click_at(&h, cell(&h, 2, 3)?)?;
    h.wait(15, |this| Ok(selections(this)?.is_empty()))?;
    let start = cell(&h, 2, 0)?;
    drag(&h, start, (start.0 + 200, start.1), true)?;
    wait_receipts(&mut h, &received, 6)?;
    h.wait(15, |this| Ok(selections(this)?.is_empty()))?;
    pass(
        results,
        "ctrl+drag from a link opens it once and does not select",
    );

    // A Ctrl+click in an unfocused pane focuses that pane before the link
    // opens. Pane 2 is focused to type into it, then pane 1 takes focus back.
    click_at(&h, cell(&h, 2, 3)?)?;
    h.wait(15, |this| Ok(focused(this)? == "PaneId(2)"))?;
    print(&h, "https://example.test/focus")?;
    click_at(&h, cell(&h, 1, 3)?)?;
    h.wait(15, |this| Ok(focused(this)? == "PaneId(1)"))?;
    h.wait(15, |this| Ok(!pane_focused(this, 2)?))?;
    ctrl_click_at(&h, cell(&h, 2, 0)?)?;
    wait_receipts(&mut h, &received, 7)?;
    h.wait(15, |this| Ok(focused(this)? == "PaneId(2)"))?;
    h.wait(15, |this| pane_focused(this, 2))?;
    h.wait(15, |this| no_menu(this))?;
    pass(
        results,
        "ctrl+click in an unfocused pane focuses it and opens the link",
    );

    h.clean_log(&h.log)?;
    h.stop(false)?;
    Ok(())
}

/// A program that enabled mouse reporting keeps every click except the one
/// that opens a link.
fn mouse_phase(binary: &Path, scratch: &Path, results: &mut Vec<String>) -> Result<()> {
    let mut h = fresh(binary, scratch, "among-menu-uri-mouse")?;
    let received = handler(&h)?;
    prepare(&mut h);
    h.start("mouse")?;
    h.wait(15, |this| Ok(this.text()?.contains("1:true:true")))?;
    let tools = std::env::current_exe()?;
    let bytes = h.base.join("mouse-link.bin");
    h.command(&format!(
        "'{}' raw-menu-pty '{}' mouse-link",
        tools.display(),
        bytes.display()
    ))?;
    h.wait(20, |_| Ok(bytes.exists()))?;
    std::thread::sleep(Duration::from_millis(700));
    if !contents(&h, "mouse-link.bin")?.is_empty() {
        return Err("mouse fixture reported input before any click".into());
    }

    // The link opens and the program is told nothing about the click.
    ctrl_click_at(&h, cell(&h, 1, 0)?)?;
    wait_receipts(&mut h, &received, 1)?;
    expect(&received, &["https://example.test/mouse-pointer"])?;
    if !contents(&h, "mouse-link.bin")?.is_empty() {
        return Err("ctrl+click on a link leaked mouse input to the program".into());
    }

    // A plain click on the same link is the program's, not the gesture's.
    click_at(&h, cell(&h, 1, 0)?)?;
    h.wait(10, |this| Ok(!contents(this, "mouse-link.bin")?.is_empty()))?;
    let after_plain = contents(&h, "mouse-link.bin")?;
    if !after_plain.windows(5).any(|bytes| bytes == b"\x1b[<0;") {
        return Err("plain click did not reach the program as a mouse report".into());
    }
    expect(&received, &["https://example.test/mouse-pointer"])?;

    // Ordinary text and a hyperlink that must not launch are the program's too.
    ctrl_click_at(&h, cell(&h, 1, 1)?)?;
    h.wait(10, |this| {
        Ok(contents(this, "mouse-link.bin")?.len() > after_plain.len())
    })?;
    let after_text = contents(&h, "mouse-link.bin")?.len();
    ctrl_click_at(&h, cell(&h, 1, 2)?)?;
    h.wait(10, |this| {
        Ok(contents(this, "mouse-link.bin")?.len() > after_text)
    })?;
    expect(&received, &["https://example.test/mouse-pointer"])?;
    pass(
        results,
        "mouse-reporting program receives plain clicks and non-openable ctrl+clicks; the link click opens",
    );

    h.key("ctrl+d")?;
    std::thread::sleep(Duration::from_millis(500));
    h.clean_log(&h.log)?;
    h.stop(false)?;
    Ok(())
}

pub fn run(binary: &Path, scratch: &Path) -> Result<PathBuf> {
    let mut supported = fresh(binary, scratch, "among-menu-uri")?;
    let base = supported.base.clone();
    let received = handler(&supported)?;
    let mut results: Vec<String> = Vec::new();
    for phase in ["supported", "failure"] {
        let mut h = if phase == "supported" {
            supported
        } else {
            fresh(binary, scratch, "among-menu-uri-failure")?
        };
        prepare(&mut h);
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
                wait_receipts(&mut h, &received, index + 1)?;
                expect(&received, &uris[..=index])?;
                wait(&mut h, |text| {
                    text.contains("1:true:true") && text.contains("terminal_menus=[]")
                })?;
            }
            h.command("printf '\\033[2J\\033[H\\033]8;;javascript:synthetic-only\\033\\\\owned-link\\033]8;;\\033\\\\n'")?;
            std::thread::sleep(Duration::from_millis(500));
            point(&h)?;
            h.xdo(&["click", "3"])?;
            wait(&mut h, |text| {
                text.contains("terminal_menus=[1]") && text.contains("Open Link:false:")
            })?;
            h.key("Escape")?;
            pass(
                &mut results,
                "http/https/mailto exact private handler arguments; unsupported OSC8 cannot launch",
            );
        } else {
            let offset = menu(&mut h, "https://example.test/no-handler")?;
            open(&mut h, offset)?;
            h.wait(10, |this| {
                Ok(this.text()?.contains("among: Could not open link:"))
            })?;
            // The pointer gesture reports the same failure, and the terminal
            // stays usable afterwards.
            let offset = h.text()?.len();
            print(&h, "https://example.test/click-no-handler")?;
            ctrl_click_at(&h, cell(&h, 1, 0)?)?;
            h.wait(10, |this| {
                Ok(this
                    .text()?
                    .get(offset..)
                    .is_some_and(|text| text.contains("among: Could not open link:")))
            })?;
            h.probe(
                "/usr/bin/import",
                &[
                    "-window",
                    "root",
                    h.base
                        .join("failure-diagnostic.png")
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
            pass(
                &mut results,
                "missing private handler reports failure; terminal accepts input without host fallback",
            );
        }
        if fs::read_to_string(&received)?.lines().count() != 3 {
            return Err("unsupported/failed URI invocation reached a handler".into());
        }
        h.clean_log(&h.log)?;
        h.stop(false)?;
        // retain ownership between loop iterations without leaking a running process.
        supported = h;
    }

    click_phases(binary, scratch, &mut results)?;
    mouse_phase(binary, scratch, &mut results)?;

    storage::atomic_write(
        &base,
        Path::new("results.json"),
        &serde_json::to_vec_pretty(&json!({"passed":results}))?,
        0o600,
    )?;
    println!("Evidence: {}", base.display());
    Ok(base)
}
