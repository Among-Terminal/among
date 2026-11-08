//! Synthetic GNOME-source, no-write, compiled-palette and restart phases.
//!
//! The application under test is the real executable. GSettings uses only a
//! private keyfile backend, a private compiled schema directory and the owned
//! harness HOME/config tree. The production dconf database and real GNOME
//! profiles are never read or written.

use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::sessions::Harness;
use crate::{storage, Result};

use super::{
    append_phase, append_samples, capture_calibrated, choose_combo, close_preferences,
    fixture_harness, open_preferences, sample_cells, terminal_json, verify_executed,
    wait_selection, wait_terminal_rects, Calibration, CellDef, Options, Oracle,
};

const TEXT_CONTROL: &str = "terminal-text-background";
const PALETTE_CONTROL: &str = "terminal-ansi-palette";
const UUID: &str = "01234567-89ab-cdef-0123-456789abcdef";
const PROFILES_PATH: &str = "/org/gnome/terminal/legacy/profiles:/";
const PROFILES_LIST_SCHEMA: &str = "org.gnome.Terminal.ProfilesList";
const PROFILE_SCHEMA: &str = "org.gnome.Terminal.Legacy.Profile";
const CUSTOM_TEXT_NOTE: &str = "custom colours";
const CUSTOM_PALETTE_NOTE: &str = "custom palette";
const GRAY_TEXT_NOTE: &str = "Gray on black";
const COMPILED_PALETTE_NOTE: &str = "compiled-in palette";

const GRAY: ([u8; 3], [u8; 3]) = ([0xaa, 0xaa, 0xaa], [0x00, 0x00, 0x00]);
// calibrate rejects a pitch below three pixels, so a four-cell calibration
// block is at least 12x12. A smaller terminal rectangle cannot contain it.
const MIN_MEASURED_TERMINAL_EXTENT: f64 = 12.0;

#[derive(Clone, Copy)]
enum Readback<'a> {
    DefaultAndPalette(&'a [[u8; 3]]),
    PaletteOnly(&'a [[u8; 3]]),
    None,
}

pub(super) fn run(options: &Options, report: &mut Value) -> Result<()> {
    report["source_cases"] = json!([]);
    let source = run_source(options, report);
    run_phase(
        report,
        "source",
        "private synthetic GNOME source, partial and invalid cases",
        source,
        &["no-write", "solarized-unavailable", "restart"],
    )?;
    let no_write = run_no_write(options, report);
    run_phase(
        report,
        "no-write",
        "opening Preferences creates no terminal.json and changes no source",
        no_write,
        &["solarized-unavailable", "restart"],
    )?;
    let solarized = run_solarized_unavailable(options, report);
    run_phase(
        report,
        "solarized-unavailable",
        "full palette tuple equals a fresh compiled-palette baseline",
        solarized,
        &["restart"],
    )?;
    let restart = run_restart(options, report);
    run_phase(
        report,
        "restart",
        "fresh restore colours, with a new process identity",
        restart,
        &[],
    )?;
    Ok(())
}

fn run_phase(
    report: &mut Value,
    name: &str,
    detail: &str,
    result: Result<()>,
    remaining: &[&str],
) -> Result<()> {
    match result {
        Ok(()) => append_phase(report, name, "passed", detail),
        Err(error) => {
            append_phase(report, name, "failed", &error.to_string())?;
            for other in remaining {
                append_phase(
                    report,
                    other,
                    "not-run",
                    "not-run after an earlier phase failure",
                )?;
            }
            Err(error)
        }
    }
}

fn run_source(options: &Options, report: &mut Value) -> Result<()> {
    let pair = custom_pair_a();
    let palette = custom_palette(0x10);
    source_case(
        options,
        report,
        &SourceCase {
            name: "source-custom",
            schema_case: "base",
            keyfile: keyfile_text(pair, &palette, false),
            pair,
            palette: &palette,
            readback: Readback::DefaultAndPalette(&palette),
            text_note: CUSTOM_TEXT_NOTE,
            palette_note: CUSTOM_PALETTE_NOTE,
        },
    )?;
    source_case(
        options,
        report,
        &SourceCase {
            name: "source-theme-partial",
            schema_case: "base",
            keyfile: keyfile_text(pair, &palette, true),
            pair: GRAY,
            palette: &palette,
            readback: Readback::DefaultAndPalette(&palette),
            text_note: GRAY_TEXT_NOTE,
            palette_note: CUSTOM_PALETTE_NOTE,
        },
    )?;

    let invalid_palette = custom_palette(0x30);
    let invalid_palette = &invalid_palette[..15];
    let baseline =
        capture_compiled_baseline(options, report, &keyfile_text(pair, invalid_palette, false))?;
    source_case(
        options,
        report,
        &SourceCase {
            name: "source-invalid-palette",
            schema_case: "base",
            keyfile: keyfile_text(pair, invalid_palette, false),
            pair,
            palette: &baseline,
            readback: Readback::PaletteOnly(invalid_palette),
            text_note: CUSTOM_TEXT_NOTE,
            palette_note: COMPILED_PALETTE_NOTE,
        },
    )?;
    source_case(
        options,
        report,
        &SourceCase {
            name: "source-missing-schema",
            schema_case: "wrong-path",
            keyfile: keyfile_text(pair, &palette, false),
            pair: GRAY,
            palette: &baseline,
            readback: Readback::None,
            text_note: GRAY_TEXT_NOTE,
            palette_note: COMPILED_PALETTE_NOTE,
        },
    )?;
    Ok(())
}

struct SourceCase<'a> {
    name: &'a str,
    schema_case: &'a str,
    keyfile: String,
    pair: ([u8; 3], [u8; 3]),
    palette: &'a [[u8; 3]],
    readback: Readback<'a>,
    text_note: &'a str,
    palette_note: &'a str,
}

fn source_case(options: &Options, report: &mut Value, case: &SourceCase<'_>) -> Result<()> {
    let mut h = fixture_harness(
        options,
        &format!("among-{}", case.name),
        "await:source\nfocus:terminal-text-background\n",
    )?;
    let result = (|| -> Result<()> {
        let schema = compile_schema(&h, options, case.schema_case)?;
        let keyfile = write_keyfile(&h, &case.keyfile)?;
        let keyfile_before = hash_file(&keyfile)?;
        configure_source(&mut h, &schema);
        verify_keyfile_readback(&h, &schema, case.readback)?;
        let pid = h.start(case.name)?;
        verify_executed(options, pid)?;
        let app_window = self::app_window(&h)?;
        wait_terminal_rects(&mut h)?;
        let script = h.base.join("start.script");
        let preferences = open_preferences(&mut h, pid, &script, "source")?;
        choose_combo(&h, preferences, TEXT_CONTROL, 2)?;
        choose_combo(&h, preferences, PALETTE_CONTROL, 2)?;
        wait_selection(&mut h, "black-on-white", "tango")?;
        choose_combo(&h, preferences, TEXT_CONTROL, 0)?;
        choose_combo(&h, preferences, PALETTE_CONTROL, 0)?;
        wait_selection(&mut h, "gnome-terminal-default", "gnome-terminal-default")?;
        check_source_notes(&h, case.text_note, case.palette_note)?;
        close_preferences(&mut h, pid, preferences, app_window)?;
        wait_preferences_closed(&mut h, pid)?;
        let (calibration, cells) = capture_calibrated(&mut h, app_window, case.name, report)?;
        sample_all(
            report,
            case.name,
            &calibration,
            &cells,
            case.pair,
            case.palette,
        )?;
        let keyfile_after = hash_file(&keyfile)?;
        if keyfile_before != keyfile_after {
            return Err("synthetic GNOME keyfile changed during the source case".into());
        }
        append_source_case(
            report,
            case.name,
            case.schema_case,
            &keyfile_before,
            case.pair,
            case.palette,
        )?;
        Ok(())
    })();
    let _ = h.stop(false);
    result
}

fn run_no_write(options: &Options, report: &mut Value) -> Result<()> {
    let mut h = fixture_harness(
        options,
        "among-no-write",
        "await:no-write\nfocus:terminal-text-background\n",
    )?;
    let result = (|| -> Result<()> {
        let schema = compile_schema(&h, options, "base")?;
        let keyfile = write_keyfile(
            &h,
            &keyfile_text(custom_pair_a(), &custom_palette(0x10), false),
        )?;
        let keyfile_before = hash_file(&keyfile)?;
        configure_source(&mut h, &schema);
        let pid = h.start("no-write")?;
        verify_executed(options, pid)?;
        let app_window = self::app_window(&h)?;
        wait_terminal_rects(&mut h)?;
        let script = h.base.join("start.script");
        let preferences = open_preferences(&mut h, pid, &script, "no-write")?;
        close_preferences(&mut h, pid, preferences, app_window)?;
        wait_preferences_closed(&mut h, pid)?;
        if terminal_json(&h)?.is_some() {
            return Err("opening Preferences on a fresh profile created terminal.json".into());
        }
        h.screenshot("no-write-fresh.png")?;
        super::append_capture(report, &h.base.join("no-write-fresh.png"))?;

        h.stop(false)?;
        h.forget_session()?;
        let terminal_path = h.base.join("config/among/terminal.json");
        let existing = json!({
            "version": 1,
            "text_background": "solarized-dark",
            "ansi_palette": "tango",
            "future": {"keep": 7},
            "external": "opaque"
        });
        storage::atomic_write(
            &h.base,
            Path::new("config/among/terminal.json"),
            &serde_json::to_vec_pretty(&existing)?,
            0o600,
        )?;
        let existing_bytes = fs::read(&terminal_path)?;
        fs::write(
            h.base.join("start.script"),
            "await:no-write-existing\nfocus:terminal-text-background\n",
        )?;
        let pid = h.start("no-write-existing")?;
        verify_executed(options, pid)?;
        let app_window = self::app_window(&h)?;
        wait_terminal_rects(&mut h)?;
        let script = h.base.join("start.script");
        let preferences = open_preferences(&mut h, pid, &script, "no-write-existing")?;
        close_preferences(&mut h, pid, preferences, app_window)?;
        wait_preferences_closed(&mut h, pid)?;
        if fs::read(&terminal_path)? != existing_bytes {
            return Err("opening Preferences changed an existing terminal.json".into());
        }
        let keyfile_after = hash_file(&keyfile)?;
        if keyfile_before != keyfile_after {
            return Err("synthetic GNOME keyfile changed during the no-write case".into());
        }
        report["no_write"] = json!({
            "fresh_document_absent": true,
            "existing_document_unchanged": true,
            "keyfile_sha256": keyfile_before,
            "screenshot": h.base.join("no-write-fresh.png").display().to_string()
        });
        Ok(())
    })();
    let _ = h.stop(false);
    result
}

fn run_solarized_unavailable(options: &Options, report: &mut Value) -> Result<()> {
    let pair = custom_pair_a();
    let source_palette = custom_palette(0x30);
    let invalid_palette = &source_palette[..15];
    let keyfile = keyfile_text(pair, invalid_palette, false);
    let baseline = capture_compiled_baseline(options, report, &keyfile)?;
    let oracle = Oracle::read(&options.oracle)?;
    let solarized = oracle.palette("solarized")?;
    let mut h = fixture_harness(
        options,
        "among-solarized-unavailable",
        "await:solarized\nfocus:terminal-text-background\nawait:solarized-reset\nfocus:terminal-text-background\n",
    )?;
    let result = (|| -> Result<()> {
        let schema = compile_schema(&h, options, "base")?;
        let keyfile_path = write_keyfile(&h, &keyfile)?;
        let keyfile_before = hash_file(&keyfile_path)?;
        configure_source(&mut h, &schema);
        let pid = h.start("solarized-unavailable")?;
        verify_executed(options, pid)?;
        let app_window = self::app_window(&h)?;
        wait_terminal_rects(&mut h)?;
        let script = h.base.join("start.script");

        let preferences = open_preferences(&mut h, pid, &script, "solarized")?;
        choose_combo(&h, preferences, PALETTE_CONTROL, 6)?;
        wait_selection(&mut h, "gnome-terminal-default", "solarized")?;
        close_preferences(&mut h, pid, preferences, app_window)?;
        wait_preferences_closed(&mut h, pid)?;
        let (calibration, cells) =
            capture_calibrated(&mut h, app_window, "solarized-explicit", report)?;
        sample_all(
            report,
            "solarized-explicit",
            &calibration,
            &cells,
            pair,
            solarized,
        )?;

        let preferences = open_preferences(&mut h, pid, &script, "solarized-reset")?;
        choose_combo(&h, preferences, PALETTE_CONTROL, 0)?;
        wait_selection(&mut h, "gnome-terminal-default", "gnome-terminal-default")?;
        close_preferences(&mut h, pid, preferences, app_window)?;
        wait_preferences_closed(&mut h, pid)?;
        let (existing_calibration, existing_cells) =
            capture_calibrated(&mut h, app_window, "solarized-reset-existing", report)?;
        sample_all(
            report,
            "solarized-reset-existing",
            &existing_calibration,
            &existing_cells,
            pair,
            &baseline,
        )?;

        let old_ids = terminal_ids(&h)?;
        h.key("ctrl+shift+t")?;
        h.wait(15, |this| {
            let text = this.text()?;
            Ok(super::latest_field(&text, "workspaces")
                .and_then(|value| value.split_whitespace().next())
                == Some("2"))
        })?;
        let (new_id, new_rect) = wait_new_terminal(&mut h, &old_ids)?;
        let (new_calibration, new_cells) =
            capture_for_terminal(&mut h, app_window, new_rect, "solarized-reset-new", report)?;
        sample_all(
            report,
            "solarized-reset-new",
            &new_calibration,
            &new_cells,
            pair,
            &baseline,
        )?;

        let keyfile_after = hash_file(&keyfile_path)?;
        if keyfile_before != keyfile_after {
            return Err("synthetic GNOME keyfile changed during the solarized case".into());
        }
        report["solarized_unavailable"] = json!({
            "baseline_pair": pair,
            "baseline_palette": baseline,
            "existing_pane": old_ids,
            "new_pane": new_id,
            "new_pane_rect": new_rect,
            "keyfile_sha256": keyfile_before
        });
        Ok(())
    })();
    let _ = h.stop(false);
    result
}

fn run_restart(options: &Options, report: &mut Value) -> Result<()> {
    run_restart_source(options, report)?;
    run_restart_saved(options, report)?;
    Ok(())
}

fn run_restart_source(options: &Options, report: &mut Value) -> Result<()> {
    let pair_a = custom_pair_a();
    let pair_b = custom_pair_b();
    let palette_a = custom_palette(0x10);
    let palette_b = custom_palette(0x50);
    let mut h = fixture_harness(options, "among-restart-source", "")?;
    let result = (|| -> Result<()> {
        let schema = compile_schema(&h, options, "base")?;
        let keyfile = write_keyfile(&h, &keyfile_text(pair_a, &palette_a, false))?;
        configure_source(&mut h, &schema);
        let first_pid = h.start("restart-source-a")?;
        verify_executed(options, first_pid)?;
        let app_window = self::app_window(&h)?;
        wait_terminal_rects(&mut h)?;
        let (calibration, cells) =
            capture_calibrated(&mut h, app_window, "restart-source-a", report)?;
        sample_all(
            report,
            "restart-source-a",
            &calibration,
            &cells,
            pair_a,
            &palette_a,
        )?;

        fs::write(&keyfile, keyfile_text(pair_b, &palette_b, false))?;
        let changed_before = hash_file(&keyfile)?;
        std::thread::sleep(std::time::Duration::from_millis(500));
        let (calibration, cells) =
            capture_calibrated(&mut h, app_window, "restart-source-live-snapshot", report)?;
        sample_all(
            report,
            "restart-source-live-snapshot",
            &calibration,
            &cells,
            pair_a,
            &palette_a,
        )?;

        h.stop(false)?;
        h.forget_session()?;
        let second_pid = h.start("restart-source-b")?;
        verify_executed(options, second_pid)?;
        if second_pid == first_pid {
            return Err("restart reused the first application PID".into());
        }
        let app_window = self::app_window(&h)?;
        wait_terminal_rects(&mut h)?;
        let (calibration, cells) =
            capture_calibrated(&mut h, app_window, "restart-source-b", report)?;
        sample_all(
            report,
            "restart-source-b",
            &calibration,
            &cells,
            pair_b,
            &palette_b,
        )?;
        let changed_after = hash_file(&keyfile)?;
        if changed_before != changed_after {
            return Err("synthetic GNOME keyfile changed during source restart".into());
        }
        report["restart_source"] = json!({
            "first_pid": first_pid,
            "second_pid": second_pid,
            "live_snapshot_pair": pair_a,
            "restart_pair": pair_b,
            "keyfile_sha256": changed_before
        });
        Ok(())
    })();
    let _ = h.stop(false);
    result
}

fn run_restart_saved(options: &Options, report: &mut Value) -> Result<()> {
    let oracle = Oracle::read(&options.oracle)?;
    let pair = oracle.scheme("black-on-white")?;
    let palette = oracle.palette("tango")?;
    let mut h = fixture_harness(
        options,
        "among-restart-saved",
        "await:restart-open\nfocus:terminal-text-background\n",
    )?;
    let result = (|| -> Result<()> {
        let schema = compile_schema(&h, options, "base")?;
        let keyfile = write_keyfile(
            &h,
            &keyfile_text(custom_pair_a(), &custom_palette(0x10), false),
        )?;
        let keyfile_before = hash_file(&keyfile)?;
        configure_source(&mut h, &schema);
        let first_pid = h.start("restart-saved")?;
        verify_executed(options, first_pid)?;
        let app_window = self::app_window(&h)?;
        wait_terminal_rects(&mut h)?;
        h.ready()?;
        let script = h.base.join("start.script");
        let preferences = open_preferences(&mut h, first_pid, &script, "restart-open")?;
        choose_combo(&h, preferences, TEXT_CONTROL, 2)?;
        choose_combo(&h, preferences, PALETTE_CONTROL, 2)?;
        wait_selection(&mut h, "black-on-white", "tango")?;
        close_preferences(&mut h, first_pid, preferences, app_window)?;
        wait_preferences_closed(&mut h, first_pid)?;
        let (calibration, cells) =
            capture_calibrated(&mut h, app_window, "restart-before", report)?;
        sample_all(
            report,
            "restart-before",
            &calibration,
            &cells,
            pair,
            palette,
        )?;

        let mut document = terminal_json(&h)?.ok_or("restart case did not create terminal.json")?;
        document["future"] = json!({"keep": 7});
        document["external"] = json!("opaque");
        storage::atomic_write(
            &h.base,
            Path::new("config/among/terminal.json"),
            &serde_json::to_vec_pretty(&document)?,
            0o600,
        )?;
        let saved_document = document.clone();

        h.key("alt+F4")?;
        h.button("Exit")?;
        h.wait(15, |this| Ok(this.snapshot_if_written().is_some()))?;
        h.stop(false)?;
        fs::write(
            h.base.join("start.script"),
            "await:restore-open\nfocus:terminal-text-background\n",
        )?;
        let second_pid = h.start("restart-restore")?;
        verify_executed(options, second_pid)?;
        if second_pid == first_pid {
            return Err("session restore reused the first application PID".into());
        }
        h.wait(20, |this| {
            let text = this.text()?;
            let ready = super::latest_field(&text, "session_ready")
                .and_then(|value| value.split_whitespace().next());
            let restore = super::latest_field(&text, "session_buttons")
                .is_some_and(|value| value.contains("Restore:"));
            Ok(ready == Some("false") && restore)
        })?;
        h.button("Restore")?;
        h.ready()?;
        let app_window = self::app_window(&h)?;
        wait_terminal_rects(&mut h)?;
        // Restore can publish the same transient rebuild rectangle as a new
        // workspace, so wait for the measured native geometry before capture.
        wait_terminal_geometry(&mut h, 1)?;
        let (calibration, cells) = capture_calibrated(&mut h, app_window, "restart-after", report)?;
        sample_all(report, "restart-after", &calibration, &cells, pair, palette)?;

        let script = h.base.join("start.script");
        let preferences = open_preferences(&mut h, second_pid, &script, "restore-open")?;
        let map = super::prefs_terminal_map(&h.text()?)?;
        let text_id = map
            .get(TEXT_CONTROL)
            .ok_or("Preferences lacks terminal-text-background after restore")?
            .0
            .as_str();
        let palette_id = map
            .get(PALETTE_CONTROL)
            .ok_or("Preferences lacks terminal-ansi-palette after restore")?
            .0
            .as_str();
        if text_id != "black-on-white" || palette_id != "tango" {
            return Err("saved appearance choices were not restored".into());
        }
        close_preferences(&mut h, second_pid, preferences, app_window)?;
        wait_preferences_closed(&mut h, second_pid)?;
        let restored = terminal_json(&h)?.ok_or("restart removed terminal.json")?;
        if restored != saved_document {
            return Err("restart changed terminal.json or its unknown keys".into());
        }
        let keyfile_after = hash_file(&keyfile)?;
        if keyfile_before != keyfile_after {
            return Err("synthetic GNOME keyfile changed during saved restart".into());
        }
        report["restart"] = json!({
            "first_pid": first_pid,
            "second_pid": second_pid,
            "text_background": "black-on-white",
            "ansi_palette": "tango",
            "unknown_keys_preserved": true,
            "keyfile_sha256": keyfile_before
        });
        Ok(())
    })();
    let _ = h.stop(false);
    result
}

fn capture_compiled_baseline(
    options: &Options,
    report: &mut Value,
    keyfile: &str,
) -> Result<[[u8; 3]; 16]> {
    let pair = custom_pair_a();
    let mut h = fixture_harness(options, "among-solarized-baseline", "")?;
    let result = (|| -> Result<[[u8; 3]; 16]> {
        let schema = compile_schema(&h, options, "base")?;
        let keyfile_path = write_keyfile(&h, keyfile)?;
        let keyfile_before = hash_file(&keyfile_path)?;
        configure_source(&mut h, &schema);
        let pid = h.start("solarized-baseline")?;
        verify_executed(options, pid)?;
        let app_window = self::app_window(&h)?;
        wait_terminal_rects(&mut h)?;
        let (calibration, cells) =
            capture_calibrated(&mut h, app_window, "solarized-baseline", report)?;
        let keep = |cell: &CellDef| cell.kind == "palette-bg";
        let (samples, _) = sample_cells(&calibration, &cells, keep, pair, &[])?;
        let mut palette = [[0u8; 3]; 16];
        for (index, slot) in palette.iter_mut().enumerate() {
            *slot = super::sample_rgb(&samples, &format!("ansi-bg-{index}"))?;
        }
        append_samples(report, "solarized-baseline", samples)?;
        let keyfile_after = hash_file(&keyfile_path)?;
        if keyfile_before != keyfile_after {
            return Err("synthetic GNOME keyfile changed during baseline capture".into());
        }
        report["solarized_baseline"] = json!({
            "pair": pair,
            "palette": palette,
            "keyfile_sha256": keyfile_before
        });
        Ok(palette)
    })();
    let _ = h.stop(false);
    result
}

fn capture_for_terminal(
    h: &mut Harness,
    app_window: u32,
    terminal: [f64; 4],
    tag: &str,
    report: &mut Value,
) -> Result<(Calibration, Vec<CellDef>)> {
    let png = h.base.join(format!("{tag}.png"));
    let raw = h.base.join(format!("{tag}.rgba"));
    h.screenshot(&format!("{tag}.png"))?;
    super::append_capture(report, &png)?;
    let image = super::decode_capture(h, app_window, &png, &raw)?;
    // The rectangle comes from the measured latest native tick, not from a
    // second log read after the screenshot. The rebuild line can carry a 2x2
    // placeholder while the window already paints the full grid.
    let calibration = super::calibrate(image, &png, &raw, terminal)?;
    super::append_calibration(report, tag, &calibration)?;
    let cells = super::read_map(&h.base.join("fixture-map.txt"))?;
    super::validate_map(&cells)?;
    Ok((calibration, cells))
}

fn sample_all(
    report: &mut Value,
    tag: &str,
    calibration: &Calibration,
    cells: &[CellDef],
    pair: ([u8; 3], [u8; 3]),
    palette: &[[u8; 3]],
) -> Result<()> {
    let keep = |cell: &CellDef| {
        matches!(
            cell.kind.as_str(),
            "default-bg" | "default-fg" | "palette-bg" | "palette-fg"
        )
    };
    let (samples, failures) = sample_cells(calibration, cells, keep, pair, palette)?;
    append_samples(report, tag, samples)?;
    if !failures.is_empty() {
        return Err(format!("{tag} samples differ: {}", failures.join("; ")).into());
    }
    Ok(())
}

fn check_source_notes(h: &Harness, text_note: &str, palette_note: &str) -> Result<()> {
    let map = super::prefs_terminal_map(&h.text()?)?;
    let text = map
        .get(TEXT_CONTROL)
        .ok_or("Preferences lacks terminal-text-background")?;
    let palette = map
        .get(PALETTE_CONTROL)
        .ok_or("Preferences lacks terminal-ansi-palette")?;
    if text.0 != "gnome-terminal-default" || palette.0 != "gnome-terminal-default" {
        return Err("source phase changed an ID away from GNOME Terminal default".into());
    }
    if !text.1.contains(text_note) {
        return Err(format!(
            "text source note does not contain {text_note:?}: {:?}",
            text.1
        )
        .into());
    }
    if !palette.1.contains(palette_note) {
        return Err(format!(
            "palette source note does not contain {palette_note:?}: {:?}",
            palette.1
        )
        .into());
    }
    Ok(())
}

fn append_source_case(
    report: &mut Value,
    name: &str,
    schema_case: &str,
    keyfile_sha256: &str,
    pair: ([u8; 3], [u8; 3]),
    palette: &[[u8; 3]],
) -> Result<()> {
    report["source_cases"]
        .as_array_mut()
        .ok_or("report source_cases is not an array")?
        .push(json!({
            "case": name,
            "schema": schema_case,
            "pair": pair,
            "palette": palette,
            "keyfile_sha256": keyfile_sha256
        }));
    Ok(())
}

fn app_window(h: &Harness) -> Result<u32> {
    h.window
        .parse::<u32>()
        .map_err(|error| format!("Harness did not publish its application window: {error}").into())
}

fn wait_preferences_closed(h: &mut Harness, pid: u32) -> Result<()> {
    h.wait(10, |this| {
        Ok(super::find_window(this, pid, super::PREFS_TITLE)?.is_none())
    })
}

fn terminal_ids(h: &Harness) -> Result<Vec<u64>> {
    let text = h.text()?;
    let entries = super::rectangle_entries(&text, "terminal_rects")?;
    terminal_ids_from_entries(entries)
}

fn terminal_ids_from_entries(entries: &str) -> Result<Vec<u64>> {
    let mut ids = Vec::new();
    for entry in entries.split_whitespace() {
        let (id, _) = entry
            .split_once(':')
            .ok_or_else(|| format!("terminal_rects entry lacks an id: {entry}"))?;
        ids.push(
            id.parse::<u64>()
                .map_err(|error| format!("terminal_rects id is not unsigned: {error}"))?,
        );
    }
    Ok(ids)
}

fn measured_terminal_rect(h: &Harness, id: u64) -> Result<[f64; 4]> {
    let rect = super::rect_of(&h.text()?, "terminal_rects", &id.to_string())?;
    if rect[2] < MIN_MEASURED_TERMINAL_EXTENT || rect[3] < MIN_MEASURED_TERMINAL_EXTENT {
        return Err(format!(
            "terminal {id} still has the transient rebuild allocation {}x{}",
            rect[2], rect[3]
        )
        .into());
    }
    Ok(rect)
}

fn wait_terminal_geometry(h: &mut Harness, id: u64) -> Result<()> {
    h.wait(15, |this| Ok(measured_terminal_rect(this, id).is_ok()))
}

fn wait_new_terminal(h: &mut Harness, old: &[u64]) -> Result<(u64, [f64; 4])> {
    let mut found = None;
    h.wait(15, |this| {
        let ids = terminal_ids(this)?;
        for id in ids.iter().copied().filter(|id| !old.contains(id)) {
            if let Ok(rect) = measured_terminal_rect(this, id) {
                found = Some((id, rect));
                return Ok(true);
            }
        }
        Ok(false)
    })?;
    found.ok_or_else(|| "new pane did not publish measured native geometry".into())
}

fn compile_schema(h: &Harness, options: &Options, case: &str) -> Result<PathBuf> {
    let source = schema_xml(options)?;
    let xml = fs::read_to_string(&source)?;
    let xml = if case == "wrong-path" {
        let replaced = xml.replace(
            "path=\"/org/gnome/terminal/legacy/profiles:/\"",
            "path=\"/org/gnome/terminal/legacy/profiles:/wrong/\"",
        );
        if replaced == xml {
            return Err("schema fixture lacks the ProfilesList path".into());
        }
        replaced
    } else {
        xml
    };
    let directory = h.base.join(format!("schemas/{case}"));
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    fs::write(directory.join("org.gnome.Terminal.gschema.xml"), xml)?;
    let directory = directory
        .to_str()
        .ok_or("schema directory is not UTF-8")?
        .to_string();
    h.probe(
        "glib-compile-schemas",
        &[
            "--strict",
            "--targetdir",
            directory.as_str(),
            directory.as_str(),
        ],
    )?;
    Ok(PathBuf::from(directory))
}

fn schema_xml(options: &Options) -> Result<PathBuf> {
    let directory = options
        .schemas
        .as_ref()
        .ok_or("source phase requires the optional gsettings schema directory")?;
    let direct = directory.join("org.gnome.Terminal.gschema.xml");
    if direct.is_file() {
        return Ok(direct);
    }
    let nested = directory.join("base/org.gnome.Terminal.gschema.xml");
    if nested.is_file() {
        return Ok(nested);
    }
    Err("terminal-gsettings schema XML is unavailable".into())
}

fn write_keyfile(h: &Harness, text: &str) -> Result<PathBuf> {
    let directory = h.base.join("config/glib-2.0/settings");
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    storage::atomic_write(
        &h.base,
        Path::new("config/glib-2.0/settings/keyfile"),
        text.as_bytes(),
        0o600,
    )?;
    Ok(directory.join("keyfile"))
}

fn configure_source(h: &mut Harness, schema: &Path) {
    h.env.insert("GSETTINGS_BACKEND".into(), "keyfile".into());
    h.env
        .insert("GSETTINGS_SCHEMA_DIR".into(), schema.as_os_str().to_owned());
}

fn verify_keyfile_readback(h: &Harness, schema: &Path, readback: Readback<'_>) -> Result<()> {
    let schema_dir = schema.to_str().ok_or("schema directory is not UTF-8")?;
    match readback {
        Readback::None => Ok(()),
        Readback::DefaultAndPalette(palette) => {
            verify_default_identity(h, schema_dir)?;
            verify_palette_readback(h, schema_dir, palette)
        }
        Readback::PaletteOnly(palette) => verify_palette_readback(h, schema_dir, palette),
    }
}

fn verify_default_identity(h: &Harness, schema_dir: &str) -> Result<()> {
    let observed = gsettings_get(h, schema_dir, PROFILES_LIST_SCHEMA, "default")?;
    let expected = format!("'{UUID}'");
    if observed.trim() != expected.as_str() {
        return Err(format!(
            "gsettings readback of the synthetic ProfilesList default returned {:?}, expected {:?}",
            observed.trim(),
            expected
        )
        .into());
    }
    Ok(())
}

fn verify_palette_readback(h: &Harness, schema_dir: &str, palette: &[[u8; 3]]) -> Result<()> {
    let schema = format!("{PROFILE_SCHEMA}:{PROFILES_PATH}:{UUID}/");
    let observed = gsettings_get(h, schema_dir, &schema, "palette")?;
    let expected = format!(
        "[{}]",
        palette
            .iter()
            .map(|rgb| format!("'{}'", colour_hex(*rgb)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if compact(&observed) != compact(&expected) {
        return Err(format!(
            "gsettings readback of the synthetic palette returned {:?}, expected {:?}",
            observed.trim(),
            expected
        )
        .into());
    }
    Ok(())
}

fn gsettings_get(h: &Harness, schema_dir: &str, schema: &str, key: &str) -> Result<String> {
    Ok(h.probe(
        "/usr/bin/gsettings",
        &["--schemadir", schema_dir, "get", schema, key],
    )
    .map_err(|error| {
        format!(
            "synthetic keyfile readback failed in {schema_dir}; gsettings must read the owned keyfile and compiled schema: {error}"
        )
    })?)
}

fn compact(value: &str) -> String {
    value.split_whitespace().collect()
}

fn colour_hex(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

fn keyfile_text(text: ([u8; 3], [u8; 3]), palette: &[[u8; 3]], theme: bool) -> String {
    let palette = palette
        .iter()
        .map(|rgb| format!("'{}'", colour_hex(*rgb)))
        .collect::<Vec<_>>()
        .join(", ");
    // The keyfile backend uses the path relative to its "/" root as the
    // group, so the group has no leading slash and the profile group has no
    // trailing slash. A leading slash here makes every read miss the group.
    format!(
        "[org/gnome/terminal/legacy/profiles:]\n\
         default='{UUID}'\n\
         list=['{UUID}']\n\n\
         [org/gnome/terminal/legacy/profiles:/:{UUID}]\n\
         foreground-color='{}'\n\
         background-color='{}'\n\
         palette=[{palette}]\n\
         use-theme-colors={theme}\n",
        colour_hex(text.0),
        colour_hex(text.1)
    )
}

fn hash_file(path: &Path) -> Result<String> {
    storage::sha256(File::open(path)?)
}

fn custom_pair_a() -> ([u8; 3], [u8; 3]) {
    ([0x12, 0x34, 0x56], [0xab, 0xcd, 0xef])
}

fn custom_pair_b() -> ([u8; 3], [u8; 3]) {
    ([0x21, 0x43, 0x65], [0xba, 0xdc, 0xfe])
}

fn custom_palette(seed: u8) -> [[u8; 3]; 16] {
    std::array::from_fn(|index| {
        let index = index as u8;
        [
            seed.wrapping_add(index),
            seed.wrapping_add(index.wrapping_mul(2)),
            seed.wrapping_add(index.wrapping_mul(3)),
        ]
    })
}

#[cfg(test)]
mod tests {
    // Exact retained rebuild line from
    // runs/colour-slice-20261108-r7/checks/among-solarized-unavailable-22-28/
    // solarized-unavailable.log. The following sidebar_rows and name_rects
    // contain a second "1:" entry that must not become a terminal id.
    const RETAINED_REBUILD_LINE: &str = concat!(
        "among[rebuild]: workspaces=2 active=1 name=\"project 2\" visible=1 panes=1 paned_widgets=1 ",
        "focused=PaneId(2) area=1590x1144 origin=320,46 tree_boxes=[2:0+0+1590x1144] ",
        "terminal_rects=[2:319+45+2x2] has_focus=true terminal_sizes=[unreported] exited=[] ",
        "sidebar_collapsed=false fullscreen=false title_buttons=(true, true) windows=1 ",
        "sidebar_origin=0,46 sidebar_rows=[0:-8+-4+16x8 1:-8+-4+16x8] ",
        "sidebar_states=[0:0:false:1 1:1:false:1] name_rects=[0:0+0+0x0 1:0+0+0x0] ",
        "names=[project|project 2] rows_in_list=2"
    );

    #[test]
    fn retained_rebuild_line_yields_only_the_terminal_pane_id() -> crate::Result<()> {
        let entries =
            crate::terminal_colours::rectangle_entries(RETAINED_REBUILD_LINE, "terminal_rects")?;
        assert_eq!(entries, "2:319+45+2x2");
        assert_eq!(
            super::terminal_ids_from_entries(entries)?,
            vec![2],
            "sidebar_rows and name_rects ids must not leak into terminal ids"
        );
        Ok(())
    }

    #[test]
    fn empty_terminal_rects_yields_no_ids() -> crate::Result<()> {
        let line = concat!(
            "among[tick1]: terminal_rects=[] has_focus=false ",
            "sidebar_rows=[0:0+0+320x40] name_rects=[0:0+0+0x0]"
        );
        let entries = crate::terminal_colours::rectangle_entries(line, "terminal_rects")?;
        assert_eq!(entries, "");
        assert!(super::terminal_ids_from_entries(entries)?.is_empty());
        Ok(())
    }
}
