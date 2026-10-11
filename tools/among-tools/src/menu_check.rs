use crate::{
    menu_geometry::{settled_row, Bounds},
    process::OwnedProcess,
    sessions::{fresh, pair, pane_rectangles as rectangles, Harness},
    storage, Result,
};
use serde_json::json;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn require(value: bool, message: &str) -> Result<()> {
    if value {
        Ok(())
    } else {
        Err(message.into())
    }
}
struct Menu {
    h: Harness,
    clipboard: PathBuf,
    runner_home: String,
    tools: PathBuf,
    providers: Vec<OwnedProcess>,
    invocation: Option<(u64, usize)>,
    results: Vec<String>,
}
impl Menu {
    fn wait(&mut self, test: impl Fn(&str) -> bool) -> Result<()> {
        self.h.wait(15, |h| {
            let text = h.text()?;
            let start = text
                .rfind("]: header_icons=")
                .and_then(|position| text[..position].rfind("among["))
                .unwrap_or(0);
            Ok(test(&text[start..]))
        })
    }
    fn bounds(&self, pane: u64) -> Result<Bounds> {
        let geometry = self.h.geometry()?;
        let origin = pair(&geometry, "origin", ',')?;
        let bounds = rectangles(&geometry, &pane.to_string())
            .last()
            .copied()
            .ok_or("menu pane bounds missing")?;
        Ok([
            origin[0] + bounds[0] as i64,
            origin[1] + bounds[1] as i64,
            bounds[2] as i64,
            bounds[3] as i64,
        ])
    }
    fn click(&self, pane: u64, button: &str, shift: bool, at: Option<(i64, i64)>) -> Result<()> {
        let [x, y, w, h] = self.bounds(pane)?;
        let (px, py) = at.unwrap_or((w / 2, (h / 2).min(120)));
        self.h.move_mouse(x + px, y + py)?;
        std::thread::sleep(Duration::from_millis(200));
        if shift {
            self.h.xdo(&["keydown", "Shift_L"])?;
        }
        let result = self.h.xdo(&["click", button]);
        if shift {
            self.h.xdo(&["keyup", "Shift_L"])?;
        }
        result?;
        std::thread::sleep(Duration::from_millis(400));
        Ok(())
    }
    fn menu(
        &mut self,
        pane: u64,
        keyboard: bool,
        shift: bool,
        at: Option<(i64, i64)>,
    ) -> Result<()> {
        let offset = self.h.text()?.len();
        self.invocation = Some((pane, offset));
        if keyboard {
            self.h.key("shift+F10")?;
        } else {
            self.click(pane, "3", shift, at)?;
        }
        self.h.wait(15, |h| {
            let text = h.text()?;
            let text = text.get(offset..).ok_or("menu log shrank")?;
            Ok(text.contains(&format!("terminal_menus=[{pane}]"))
                && text.contains("Copy:")
                && text.contains("icon:"))
        })
    }
    fn action(&mut self, label: &str) -> Result<()> {
        let (pane, offset) = self
            .invocation
            .ok_or("action requires a live menu invocation")?;
        self.h.wait(15, |h| {
            Ok(settled_row(
                h.text()?.get(offset..).ok_or("menu log shrank")?,
                pane,
                label,
            )?
            .is_some())
        })?;
        let bounds = settled_row(
            self.h.text()?.get(offset..).ok_or("menu log shrank")?,
            pane,
            label,
        )?
        .ok_or("settled action bounds missing")?;
        let mut trace = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.h.base.join("action-coordinates.jsonl"))?;
        writeln!(
            trace,
            "{}",
            json!({"pane":pane,"label":label,"invocation_offset":offset,"bounds":bounds,"click":[bounds[0]+bounds[2]/2,bounds[1]+bounds[3]/2]})
        )?;
        self.h
            .move_mouse(bounds[0] + bounds[2] / 2, bounds[1] + bounds[3] / 2)?;
        std::thread::sleep(Duration::from_millis(200));
        self.h.xdo(&["click", "1"])?;
        std::thread::sleep(Duration::from_millis(500));
        Ok(())
    }
    fn command(&self, text: &str) -> Result<()> {
        self.h.command(text)?;
        std::thread::sleep(Duration::from_millis(400));
        Ok(())
    }
    fn clipboard(&mut self, mode: &str, value: Option<&str>) -> Result<String> {
        let path = self
            .clipboard
            .to_str()
            .ok_or("clipboard helper path is not UTF-8")?;
        let mut args = vec![self.runner_home.as_str(), mode];
        if let Some(value) = value {
            args.push(value);
        }
        if mode == "read" {
            return self.h.probe(path, &args);
        }
        let log = File::create(
            self.h
                .base
                .join(format!("clipboard-provider-{}.log", self.providers.len())),
        )?;
        let mut provider =
            OwnedProcess::spawn(&self.clipboard, &args, &self.h.env, &self.h.base, &log)?;
        std::thread::sleep(Duration::from_millis(500));
        require(provider.is_running()?, "private clipboard provider failed")?;
        self.providers.push(provider);
        Ok(String::new())
    }
    fn raw(&mut self, pane: u64, file: &str, mode: &str) -> Result<()> {
        self.click(pane, "1", false, None)?;
        self.command(&format!(
            "'{}' raw-menu-pty '{}' {mode}",
            self.tools.display(),
            self.h.base.join(file).display()
        ))?;
        self.h.wait(8, |h| Ok(h.base.join(file).exists()))
    }
    fn check(&mut self, name: &str) {
        println!("PASS {name}");
        self.results.push(name.into());
    }
    fn shot(&self, name: &str) -> Result<()> {
        self.h.probe(
            "/usr/bin/import",
            &[
                "-window",
                "root",
                self.h
                    .base
                    .join(name)
                    .to_str()
                    .ok_or("screenshot path is not UTF-8")?,
            ],
        )?;
        Ok(())
    }
}

pub fn run(binary: &Path, clipboard: &Path, theme: &str, scratch: &Path) -> Result<PathBuf> {
    crate::preferences::owned_display()?;
    require(
        matches!(theme, "light" | "dark"),
        "menu theme must be light or dark",
    )?;
    // installed Snap acceptance is a release-only boundary, not a source CLI escape.
    require(
        !binary.starts_with("/snap"),
        "installed Snap menu checks require check-context-menu-snap",
    )?;
    run_selected(binary, clipboard, theme, scratch, false)
}

pub fn run_snap(
    clipboard: &Path,
    theme: &str,
    scratch: &Path,
    github_hosted: bool,
) -> Result<PathBuf> {
    crate::preferences::owned_display()?;
    let virtualization = std::process::Command::new("/usr/bin/systemd-detect-virt")
        .env_clear()
        .output()?;
    let guest = matches!(
        std::str::from_utf8(&virtualization.stdout)?.trim(),
        "kvm" | "qemu"
    );
    let approved = crate::platform::hosted_rehearsal(
        github_hosted,
        std::env::var("GITHUB_ACTIONS").ok().as_deref(),
        std::env::var("RUNNER_ENVIRONMENT").ok().as_deref(),
    );
    require((guest||approved)&&Path::new("/snap/among/current/bin/among").is_file(),"installed menu checks require a disposable guest or explicitly approved GitHub-hosted runner with the among Snap")?;
    run_selected(
        Path::new("/snap/bin/among"),
        clipboard,
        theme,
        scratch,
        true,
    )
}

fn run_selected(
    binary: &Path,
    clipboard: &Path,
    theme: &str,
    scratch: &Path,
    installed: bool,
) -> Result<PathBuf> {
    require(
        matches!(theme, "light" | "dark"),
        "menu theme must be light or dark",
    )?;
    let mut h = fresh(binary, scratch, "among-menu")?;
    fs::write(
        h.base.join("config/among/shortcuts.json"),
        b"{\"version\":1,\"overrides\":{\"close-pane\":null}}",
    )?;
    h.env.insert("AMONG_COLOR_SCHEME".into(), theme.into());
    h.env
        .insert("AMONG_REBIND".into(), "split-pane=<Control><Shift>b".into());
    let mut m = Menu {
        h,
        clipboard: clipboard.canonicalize()?,
        runner_home: std::env::var("HOME")?,
        tools: std::env::current_exe()?,
        providers: Vec::new(),
        invocation: None,
        results: Vec::new(),
    };
    let pid = m.h.start("app")?;
    if installed {
        let maps = fs::read_to_string(format!("/proc/{pid}/maps"))?;
        require(
            maps.contains("/snap/among/")
                && maps.contains("libgtk-4")
                && maps.contains("libvte-2.91-gtk4"),
            "installed menu process is not bound to Snap GTK/VTE payload",
        )?;
        fs::write(m.h.base.join("installed-maps.txt"), maps)?;
    }
    m.wait(|text| text.contains("1:true:true"))?;
    m.clipboard("binary", None)?;
    m.menu(1, true, false, None)?;
    m.wait(|text| text.contains("icon:") && text.contains("edit-copy-symbolic"))?;
    m.shot("menu-icons.png")?;
    let text = m.h.text()?;
    require(
        text.contains("Copy:false:")
            && text.contains("Paste:false:")
            && !text.contains("Open Link:"),
        "binary clipboard/empty link menu state wrong",
    )?;
    m.h.key("Escape")?;
    m.wait(|text| text.contains("terminal_menus=[]") && text.contains("1:true:true"))?;
    m.check("keyboard menu, disabled clipboard actions and Escape focus return");
    m.command("printf 'owned pane-one selection\\n'")?;
    m.menu(1, true, false, None)?;
    m.h.key("Down")?;
    m.h.key("Up")?;
    m.h.key("Return")?;
    m.wait(|text| text.contains("terminal_selections=[1]"))?;
    m.menu(1, false, false, None)?;
    m.action("Copy")?;
    require(
        m.clipboard("read", None)?
            .contains("owned pane-one selection"),
        "menu copy lost real selection",
    )?;
    m.menu(1, false, false, None)?;
    m.action("Split Pane")?;
    m.wait(|text| text.contains("panes=2") && text.contains("2:true:true"))?;
    m.menu(1, false, false, None)?;
    m.action("Select All")?;
    m.wait(|text| text.contains("terminal_selections=[1]"))?;
    m.click(2, "1", false, None)?;
    m.wait(|text| text.contains("2:true:true") && text.contains("terminal_selections=[1]"))?;
    m.menu(1, false, false, None)?;
    m.wait(|text| {
        text.contains("terminal_menus=[1]")
            && text.contains("focused=PaneId(1)")
            && text.contains("terminal_selections=[1]")
    })?;
    m.h.key("Escape")?;
    for _ in 0..4 {
        m.menu(1, true, false, None)?;
        m.h.key("Escape")?;
    }
    m.check("real selection survives unfocused right-click and repeated native popovers");
    m.raw(1, "source.bin", "source")?;
    m.menu(1, true, false, None)?;
    m.action("Select All")?;
    m.wait(|text| text.contains("terminal_selections=[1]"))?;
    m.menu(1, true, false, None)?;
    m.action("Copy")?;
    let copied = m.clipboard("read", None)?;
    require(
        copied.contains("owned source first line\nowned source second line\n")
            && copied.contains("owned pane-one selection"),
        "Select All did not copy source and scrollback",
    )?;
    require(
        fs::read(m.h.base.join("source.bin"))?.is_empty(),
        "copy sent data to source PTY",
    )?;
    m.raw(2, "paste.bin", "paste")?;
    m.menu(2, false, false, None)?;
    m.action("Paste")?;
    let mut expected = format!("\x1b[200~{}\x1b[201~", copied.replace('\n', "\r")).into_bytes();
    require(
        fs::read(m.h.base.join("paste.bin"))? == expected
            && fs::read(m.h.base.join("source.bin"))?.is_empty(),
        "cross-pane paste differs or leaked into source",
    )?;
    m.check("Select All/Copy in A paste exact bytes only into B without replay");
    m.clipboard("set", Some("synthetic first line\nsynthetic second line"))?;
    m.menu(2, false, false, None)?;
    m.action("Paste")?;
    expected.extend_from_slice(b"\x1b[200~synthetic first line\rsynthetic second line\x1b[201~");
    require(
        fs::read(m.h.base.join("paste.bin"))? == expected
            && fs::read(m.h.base.join("source.bin"))?.is_empty(),
        "synthetic bracketed paste bytes differ",
    )?;
    m.h.key("ctrl+d")?;
    m.click(1, "1", false, None)?;
    m.h.key("ctrl+d")?;
    m.click(2, "1", false, None)?;
    m.check("requesting raw PTY receives exact bracketed paste bytes");
    m.raw(2, "mouse.bin", "mouse")?;
    let offset = m.h.text()?.len();
    m.click(2, "3", false, None)?;
    std::thread::sleep(Duration::from_millis(1200));
    require(
        m.h.text()?
            .get(offset..)
            .is_some_and(|text| text.contains("terminal_menus=[]")),
        "TUI right-click opened host menu",
    )?;
    let mouse = fs::read(m.h.base.join("mouse.bin"))?;
    require(
        mouse.windows(5).any(|bytes| bytes == b"\x1b[<2;"),
        "TUI did not receive right-click",
    )?;
    m.menu(2, false, true, None)?;
    require(
        fs::read(m.h.base.join("mouse.bin"))? == mouse,
        "Shift override leaked mouse input",
    )?;
    m.h.key("Escape")?;
    m.menu(2, true, false, None)?;
    require(
        fs::read(m.h.base.join("mouse.bin"))? == mouse,
        "keyboard menu leaked PTY input",
    )?;
    m.h.key("Escape")?;
    m.h.key("ctrl+d")?;
    m.check("TUI receives ordinary right-click; Shift and keyboard override without PTY input");
    m.click(1, "1", false, None)?;
    m.command("printf '\\033[2J\\033[H\\033]8;;https://example.test/owned-a\\033\\\\https://display.test/not-the-target\\033]8;;\\033\\\\\\n'")?;
    m.click(2, "1", false, None)?;
    m.command("printf '\\033[2J\\033[Hhttps://example.test/owned-b\\n'")?;
    m.menu(1, false, false, Some((36, 38)))?;
    let bounds = m.bounds(2)?;
    m.h.move_mouse(bounds[0] + 36, bounds[1] + 38)?;
    m.action("Copy Link")?;
    require(
        m.clipboard("read", None)? == "https://example.test/owned-a",
        "captured OSC8 target changed after pointer movement",
    )?;
    m.click(2, "1", false, None)?;
    m.menu(2, true, false, None)?;
    m.wait(|text| {
        text.contains("terminal_menus=[2]")
            && !text.contains("Open Link:")
            && !text.contains("Copy Link:")
    })?;
    m.h.key("Escape")?;
    m.menu(2, false, false, Some((36, 38)))?;
    m.action("Copy Link")?;
    require(
        m.clipboard("read", None)? == "https://example.test/owned-b",
        "second-pane pointer target wrong",
    )?;
    m.check("per-pane coordinates and captured OSC8 beat display text and later pointer movement");
    m.command("printf '\\033[2J\\033[Hhttps://example.test/path?a=1&b=2.\\n'")?;
    m.menu(2, false, false, Some((36, 38)))?;
    m.action("Copy Link")?;
    require(
        m.clipboard("read", None)? == "https://example.test/path?a=1&b=2",
        "plain URL copied punctuation",
    )?;
    m.menu(2, true, false, None)?;
    m.wait(|text| text.contains("terminal_menus=[2]") && !text.contains("Open Link:"))?;
    m.h.key("Escape")?;
    m.command("printf '\\033[2J\\033[H\\033]8;;custom:synthetic-target\\033\\\\owned-link\\033]8;;\\033\\\\\\n'")?;
    m.menu(2, false, false, Some((36, 38)))?;
    m.wait(|text| text.contains("Open Link:false:"))?;
    m.shot("link-icons.png")?;
    m.action("Copy Link")?;
    require(
        m.clipboard("read", None)? == "custom:synthetic-target",
        "unsupported URI is not copy-only",
    )?;
    m.check(
        "plain URL punctuation stripped; unsupported explicit URI copy-only; keyboard omits links",
    );
    m.click(1, "1", false, None)?;
    m.wait(|text| text.contains("1:true:true"))?;
    let untouched = m.bounds(1)?;
    m.menu(2, false, false, None)?;
    m.action("Split Pane")?;
    m.wait(|text| text.contains("visible=3 panes=3") && text.contains("3:true:true"))?;
    require(
        m.bounds(1)? == untouched,
        "unfocused split changed previously focused pane",
    )?;
    m.command("printf '\\033]0;nested-new-pane\\007'")?;
    m.wait(|text| text.contains("3|nested-new-pane|"))?;
    m.check("unfocused target splits nested layout, preserves A and accepts typing");
    m.click(1, "1", false, None)?;
    m.menu(3, false, false, None)?;
    m.action("Expand Pane")?;
    m.wait(|text| text.contains("visible=1 panes=3") && text.contains("3:true:true"))?;
    m.menu(3, true, false, None)?;
    m.action("Restore Pane")?;
    m.wait(|text| {
        text.starts_with("among[tick")
            && text.contains("visible=3 panes=3")
            && text.contains("3:true:true")
    })?;
    require(
        m.bounds(1)? == untouched,
        "nested zoom restore changed untouched pane",
    )?;
    m.click(1, "1", false, None)?;
    m.menu(3, false, false, None)?;
    m.action("Expand Pane")?;
    m.wait(|text| text.contains("visible=1 panes=3"))?;
    m.menu(3, true, false, None)?;
    m.action("Close Terminal")?;
    m.wait(|text| text.contains("visible=2 panes=2") && text.contains("2:true:true"))?;
    m.command("printf '\\033]0;nested-zoom-survivor\\007'")?;
    m.wait(|text| text.contains("2|nested-zoom-survivor|"))?;
    m.check("nested zoom restore/close transfers usable focus to live sibling");
    m.menu(2, false, false, None)?;
    m.action("Expand Pane")?;
    m.wait(|text| text.contains("visible=1 panes=2"))?;
    m.menu(2, true, false, None)?;
    m.action("Restore Pane")?;
    m.wait(|text| text.starts_with("among[tick") && text.contains("visible=2 panes=2"))?;
    m.menu(2, false, false, None)?;
    m.action("Close Terminal")?;
    m.wait(|text| text.contains("panes=1") && text.contains("1:true:true"))?;
    m.command("printf '\\033]0;menu-survivor\\007'")?;
    m.wait(|text| text.contains("menu-survivor"))?;
    m.check("zoom/restore and unbound close preserve survivor input");
    m.menu(1, false, false, None)?;
    m.action("Close Terminal")?;
    std::thread::sleep(Duration::from_millis(600));
    m.shot("exit-confirmation.png")?;
    m.h.key("Escape")?;
    m.wait(|text| text.contains("1:true:true"))?;
    m.command("printf '\\033]0;cancelled-exit-survivor\\007'")?;
    m.wait(|text| text.contains("1|cancelled-exit-survivor|"))?;
    m.check("last-pane close uses cancellable exit confirmation");
    m.h.screenshot("window.png")?;
    m.h.clean_log(&m.h.log)?;
    for mode in ["layout", "workspace", "exit"] {
        m.h.stop(false)?;
        let session = m.h.base.join("data/among/session.json");
        if session.exists() {
            fs::remove_file(session)?;
        }
        let steps = if mode == "exit" {
            "split-pane\ntype:sleep~4;~exit\n".into()
        } else {
            format!(
                "{}{}\n",
                "type:true\n".repeat(10),
                if mode == "layout" {
                    "split-pane"
                } else {
                    "new-workspace"
                }
            )
        };
        let script = m.h.base.join(format!("{mode}.script"));
        fs::write(&script, steps)?;
        m.h.env
            .insert("AMONG_SCRIPT".into(), script.into_os_string());
        m.h.start(&format!("async-{mode}"))?;
        m.wait(|text| text.contains("1:true:true") || text.contains("2:true:true"))?;
        let survivor = if mode == "exit" {
            m.wait(|text| text.contains("visible=2 panes=2") && text.contains("2:true:true"))?;
            m.menu(2, true, false, None)?;
            m.wait(|text| {
                text.contains("visible=1 panes=1")
                    && text.contains("1:true:true")
                    && text.contains("terminal_menus=[]")
            })?;
            1
        } else {
            m.menu(1, true, false, None)?;
            m.wait(|text| {
                text.contains("2:true:true")
                    && text.contains("terminal_menus=[]")
                    && text.contains(if mode == "layout" {
                        "visible=2 panes=2"
                    } else {
                        "workspaces=2 active=1"
                    })
            })?;
            2
        };
        m.command(&format!("printf '\\033]0;async-{mode}-survivor\\007'"))?;
        m.wait(|text| text.contains(&format!("{survivor}|async-{mode}-survivor|")))?;
        if mode == "workspace" {
            m.h.key("ctrl+Page_Up")?;
            m.wait(|text| {
                text.contains("workspaces=2 active=0")
                    && text.contains("1:true:true")
                    && text.contains("terminal_menus=[]")
            })?;
            m.command("printf '\\033]0;returned-workspace-input\\007'")?;
            m.wait(|text| text.contains("1|returned-workspace-input|"))?;
        }
        m.h.clean_log(&m.h.log)?;
        m.check(&format!(
            "asynchronous {mode} revokes old menu without stealing survivor input"
        ));
    }
    m.h.stop(false)?;
    for provider in &mut m.providers {
        provider.cleanup()?;
    }
    storage::atomic_write(
        &m.h.base,
        Path::new("results.json"),
        &serde_json::to_vec_pretty(&json!({"passed":m.results}))?,
        0o600,
    )?;
    println!("Evidence: {}", m.h.base.display());
    Ok(m.h.base.clone())
}
