use crate::{
    sessions::{fresh, pair, pane_rectangles as rectangles, Harness},
    Result,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn focus(h: &mut Harness, pane: u64, offset: usize) -> Result<()> {
    h.wait(12, |this| {
        let text = this.text()?;
        let text = text.get(offset..).ok_or("focus log shrank")?;
        Ok(text.contains(&format!("focused=PaneId({pane})"))
            && text.contains(&format!("{pane}:true:true")))
    })?;
    let text = h.text()?;
    let row = text
        .get(offset..)
        .ok_or("focus log shrank")?
        .rfind("pane_focus_states=[")
        .and_then(|position| text.get(offset + position + "pane_focus_states=[".len()..))
        .and_then(|tail| tail.split(']').next())
        .ok_or("pane focus states missing")?;
    if row.matches(":true\"").count() != 1 {
        return Err("native focus has multiple/zero highlighted panes".into());
    }
    Ok(())
}

/// Wait for the expanded state to prove three things at once: only `pane` is
/// rendered, it owns the model focus, and it still holds native keyboard focus.
///
/// The pane count is part of the assertion because a split that slipped through
/// would grow it, and the focus row catches a directional move to a hidden pane.
fn expanded_focus(h: &mut Harness, pane: u64, offset: usize) -> Result<()> {
    h.wait(12, |this| {
        let text = this.text()?;
        let text = text.get(offset..).ok_or("zoom log shrank")?;
        Ok(text.contains("visible=1 panes=2")
            && text.contains(&format!("focused=PaneId({pane})"))
            && text.contains(&format!("{pane}:true:true")))
    })?;
    focus(h, pane, offset)
}

fn marker(h: &mut Harness, pane: u64, name: &str) -> Result<()> {
    let offset = h.text()?.len();
    h.command(&format!("printf \"\\033]0;{name}\\007\""))?;
    h.wait(12, |this| {
        Ok(this
            .text()?
            .get(offset..)
            .is_some_and(|text| text.contains(&format!("{pane}|{name}|false"))))
    })
}

fn click(h: &Harness, pane: u64, header: bool) -> Result<()> {
    let geometry = h.geometry()?;
    let origin = pair(&geometry, "origin", ',')?;
    let bounds = rectangles(&geometry, &pane.to_string())
        .last()
        .copied()
        .ok_or("pane has no native geometry")?;
    h.move_mouse(
        origin[0] + (bounds[0] + bounds[2] / 2.0) as i64,
        origin[1]
            + bounds[1] as i64
            + if header {
                12
            } else {
                80.min(bounds[3] as i64 / 2)
            },
    )?;
    h.xdo(&["click", "1"])
}

pub fn run(binary: &Path, scratch: &Path) -> Result<PathBuf> {
    let mut h = fresh(binary, scratch, "among-focus")?;
    h.env.insert("AMONG_SELFCHECK_TICKS".into(), "500".into());
    h.start("app")?;
    h.wait(12, |this| Ok(this.text()?.contains("has_focus=true")))?;
    std::thread::sleep(Duration::from_millis(500));
    h.key("ctrl+shift+n")?;
    h.wait(12, |this| {
        let text = this.text()?;
        Ok(text.contains("panes=2") && text.contains("2:true:true"))
    })?;
    let offset = h.text()?.len();
    h.key("ctrl+shift+x")?;
    expanded_focus(&mut h, 2, offset)?;
    println!("PASS zoom renders only the focused pane");
    for key in ["alt+Left", "alt+Right", "alt+Up", "alt+Down"] {
        let offset = h.text()?.len();
        h.key(key)?;
        expanded_focus(&mut h, 2, offset)?;
        println!("PASS {key} leaves focus on the expanded pane");
    }
    let offset = h.text()?.len();
    h.key("ctrl+shift+n")?;
    expanded_focus(&mut h, 2, offset)?;
    println!("PASS split is refused while a pane is expanded");
    let offset = h.text()?.len();
    h.key("ctrl+shift+x")?;
    h.wait(12, |this| {
        let text = this.text()?;
        let text = text.get(offset..).ok_or("restore log shrank")?;
        Ok(text.contains("visible=2 panes=2")
            && text.contains("focused=PaneId(2)")
            && text.contains("2:true:true"))
    })?;
    println!("PASS restore keeps focus on the formerly expanded pane");
    let offset = h.text()?.len();
    h.key("alt+Left")?;
    focus(&mut h, 1, offset)?;
    println!("PASS directional focus resumes after restore");
    let offset = h.text()?.len();
    click(&h, 1, false)?;
    focus(&mut h, 1, offset)?;
    marker(&mut h, 1, "body-click-one")?;
    println!("PASS body click synchronizes terminal/model/highlight");
    let offset = h.text()?.len();
    click(&h, 2, true)?;
    focus(&mut h, 2, offset)?;
    marker(&mut h, 2, "header-click-two")?;
    println!("PASS header click synchronizes terminal/model/highlight");
    h.command("printf 'selection sample text\\n%.0s' {1..8}")?;
    std::thread::sleep(Duration::from_secs(1));
    let geometry = h.geometry()?;
    let origin = pair(&geometry, "origin", ',')?;
    let bounds = rectangles(&geometry, "2")
        .last()
        .copied()
        .ok_or("second pane geometry missing")?;
    let offset = h.text()?.len();
    h.move_mouse(
        origin[0] + bounds[0] as i64 + 25,
        origin[1] + bounds[1] as i64 + 60,
    )?;
    h.xdo(&["mousedown", "1"])?;
    h.move_mouse(
        origin[0] + bounds[0] as i64 + 160,
        origin[1] + bounds[1] as i64 + 60,
    )?;
    h.xdo(&["mouseup", "1"])?;
    h.wait(12, |this| {
        Ok(this
            .text()?
            .get(offset..)
            .is_some_and(|text| text.contains("terminal_selections=[2]")))
    })?;
    println!("PASS real terminal drag selection");
    for (pane, mode, name) in [
        (3, "manual", "manual-close-survivor"),
        (4, "exit", "exit-survivor"),
        (5, "zoom", "zoom-close-survivor"),
    ] {
        h.key("ctrl+shift+n")?;
        h.wait(12, |this| {
            Ok(this.text()?.contains(&format!("{pane}:true:true")))
        })?;
        if mode == "zoom" {
            h.key("ctrl+shift+x")?;
        }
        let offset = h.text()?.len();
        if mode == "exit" {
            h.command("exit")?;
        } else {
            h.key("ctrl+shift+w")?;
        }
        focus(&mut h, 2, offset)?;
        marker(&mut h, 2, name)?;
        println!("PASS {mode} close restores sibling keyboard focus");
    }
    h.command("sleep 3; exit")?;
    h.key("ctrl+shift+t")?;
    h.wait(12, |this| Ok(this.text()?.contains("6:true:true")))?;
    std::thread::sleep(Duration::from_secs(4));
    let offset = h.text()?.len();
    marker(&mut h, 6, "background-exit-preserves-active")?;
    focus(&mut h, 6, offset)?;
    println!("PASS background exit preserves active workspace keyboard focus");
    h.clean_log(&h.log)?;
    h.stop(false)?;
    println!("Evidence: {}", h.base.display());
    Ok(h.base.clone())
}
