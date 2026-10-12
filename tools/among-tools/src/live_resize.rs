//! Keyboard divider movement in a running window, driven by real key events.
//!
//! `Ctrl+Shift` with an arrow must move the divider next to the focused pane in
//! the arrow's direction. The unit tests prove the tree arithmetic; only a live
//! window proves that GTK delivers all four accelerators to the action and that
//! the widgets follow the tree.

use crate::{
    sessions::{fresh, pane_rectangles, Harness},
    Result,
};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The latest native rectangle `[x, y, width, height]` of `pane`.
fn bounds(h: &Harness, pane: u64) -> Result<[f64; 4]> {
    pane_rectangles(&h.geometry()?, &pane.to_string())
        .last()
        .copied()
        .ok_or_else(|| format!("pane {pane} has no native geometry").into())
}

/// Press `key` and wait until `measure` of `pane` has changed in the expected
/// direction by more than a few pixels.
///
/// A divider step is 5% of the split, far more than rounding noise, so a
/// change smaller than `MIN_STEP_PX` is treated as "did not move".
fn expect_move(
    h: &mut Harness,
    key: &str,
    pane: u64,
    measure: fn([f64; 4]) -> f64,
    grows: bool,
) -> Result<()> {
    const MIN_STEP_PX: f64 = 8.0;
    let before = measure(bounds(h, pane)?);
    h.key(key)?;
    h.wait(12, |this| {
        let now = measure(bounds(this, pane)?);
        Ok(if grows {
            now > before + MIN_STEP_PX
        } else {
            now < before - MIN_STEP_PX
        })
    })
    .map_err(|_| {
        format!("{key} did not move the divider beside pane {pane} (was {before:.0}px)").into()
    })
}

fn width(rect: [f64; 4]) -> f64 {
    rect[2]
}

fn height(rect: [f64; 4]) -> f64 {
    rect[3]
}

pub fn run(binary: &Path, scratch: &Path) -> Result<PathBuf> {
    let mut h = fresh(binary, scratch, "among-resize")?;
    h.env.insert("AMONG_SELFCHECK_TICKS".into(), "500".into());
    h.start("app")?;
    h.wait(12, |this| Ok(this.text()?.contains("has_focus=true")))?;
    std::thread::sleep(Duration::from_millis(500));

    // A maximized window is wider than tall, so the first split is side by
    // side and focus lands on the new right-hand pane 2.
    h.key("ctrl+shift+n")?;
    h.wait(12, |this| {
        let text = this.text()?;
        Ok(text.contains("panes=2") && text.contains("2:true:true"))
    })?;
    let left = bounds(&h, 1)?;
    let right = bounds(&h, 2)?;
    if right[0] <= left[0] || (right[1] - left[1]).abs() > 1.0 {
        return Err(format!("first split is not side by side: {left:?} {right:?}").into());
    }
    // From the right-hand pane, Right must widen the LEFT pane: the divider
    // goes right. The grow/shrink implementation moved it left here.
    expect_move(&mut h, "ctrl+shift+Right", 1, width, true)?;
    println!("PASS Ctrl+Shift+Right from the right pane moves the divider right");
    expect_move(&mut h, "ctrl+shift+Left", 1, width, false)?;
    println!("PASS Ctrl+Shift+Left moves the divider left");

    // Pane 2 is now narrower than tall, so splitting it stacks pane 3 below.
    h.key("ctrl+shift+n")?;
    h.wait(12, |this| Ok(this.text()?.contains("3:true:true")))?;
    let upper = bounds(&h, 2)?;
    let lower = bounds(&h, 3)?;
    if lower[1] <= upper[1] || (lower[0] - upper[0]).abs() > 1.0 {
        return Err(format!("second split is not stacked: {upper:?} {lower:?}").into());
    }
    // From the lower pane, Down must make the UPPER pane taller.
    expect_move(&mut h, "ctrl+shift+Down", 2, height, true)?;
    println!("PASS Ctrl+Shift+Down moves the stacked divider down");
    expect_move(&mut h, "ctrl+shift+Up", 2, height, false)?;
    println!("PASS Ctrl+Shift+Up moves the stacked divider up");
    // The same pane is also bounded by the outer side-by-side divider.
    expect_move(&mut h, "ctrl+shift+Left", 1, width, false)?;
    println!("PASS Ctrl+Shift+Left from a stacked pane moves the outer divider");

    h.clean_log(&h.log)?;
    h.stop(false)?;
    println!("Evidence: {}", h.base.display());
    Ok(h.base.clone())
}
