//! Deterministic colour fixture installed as Among's `SHELL`.
//!
//! It paints a calibration block and a fixed SGR grid on the real PTY, records
//! the exact bytes it emitted and a label -> grid-position map, then replaces
//! itself with an interactive bash on the same PTY. Keeping bash is what makes
//! real typing, scrollback and selection tests possible without a second helper.
//!
//! `terminal-colour-fixture redraw` paints the grid again and exits; the
//! checker types that command after scrolling the history.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const BLOCK: &str = "\x1b[48;2;1;2;3m  \x1b[0m";
const BLOCK_SIZE: u32 = 4;

struct Cell {
    label: String,
    row: u32,
    col: u32,
    kind: &'static str,
    index: u32,
}

macro_rules! push_cell {
    ($bytes:expr, $map:expr, $label:expr, $row:expr, $col:expr, $sgr:expr, $kind:expr, $index:expr $(,)?) => {{
        $bytes.extend_from_slice($sgr.as_bytes());
        $bytes.extend_from_slice(b"  ");
        $bytes.extend_from_slice(b"\x1b[0m");
        $map.push(Cell {
            label: $label,
            row: $row,
            col: $col,
            kind: $kind,
            index: $index,
        });
    }};
}

fn grid() -> (Vec<u8>, Vec<Cell>) {
    let mut bytes = b"\x1b[2J\x1b[H\x1b[?25l".to_vec();
    let mut map = Vec::new();

    // Four by four cells of one unusual opaque colour. The checker finds this
    // block in the PNG and derives the exact cell pitch and origin from it.
    for _ in 0..BLOCK_SIZE {
        for _ in 0..BLOCK_SIZE {
            bytes.extend_from_slice(BLOCK.as_bytes());
        }
        bytes.extend_from_slice(b"\r\n");
    }
    map.push(Cell {
        label: "calibration-block".into(),
        row: 0,
        col: 0,
        kind: "block",
        index: BLOCK_SIZE,
    });

    // Row 4: ANSI background indices 0..15, written as direct backgrounds.
    let row = 4;
    for index in 0..16u32 {
        push_cell!(
            &mut bytes,
            &mut map,
            format!("ansi-bg-{index}"),
            row,
            index,
            &format!("\x1b[48;5;{index}m"),
            "palette-bg",
            index,
        );
    }
    bytes.extend_from_slice(b"\r\n");

    // Row 5: the same indices as reverse-video spaces, which expose foreground
    // colour without depending on glyph edges.
    let row = 5;
    for index in 0..16u32 {
        push_cell!(
            &mut bytes,
            &mut map,
            format!("ansi-fg-{index}"),
            row,
            index,
            &format!("\x1b[38;5;{index};7m"),
            "palette-fg",
            index,
        );
    }
    bytes.extend_from_slice(b"\r\n");

    // Row 6: defaults, reset, SGR forms and colours outside the 16-entry
    // palette. Extended indices are VTE's compiled values, not palette entries.
    let row = 6;
    push_cell!(
        &mut bytes,
        &mut map,
        "default-bg".into(),
        row,
        0,
        "\x1b[49m",
        "default-bg",
        0,
    );
    push_cell!(
        &mut bytes,
        &mut map,
        "default-fg".into(),
        row,
        1,
        "\x1b[39;7m",
        "default-fg",
        0,
    );
    push_cell!(
        &mut bytes,
        &mut map,
        "reset-bg".into(),
        row,
        2,
        "\x1b[0m\x1b[49m",
        "default-bg",
        0,
    );
    push_cell!(
        &mut bytes,
        &mut map,
        "reset-fg".into(),
        row,
        3,
        "\x1b[0m\x1b[39;7m",
        "default-fg",
        0,
    );
    push_cell!(
        &mut bytes,
        &mut map,
        "normal-34".into(),
        row,
        4,
        "\x1b[34;7m",
        "palette-fg",
        4,
    );
    push_cell!(
        &mut bytes,
        &mut map,
        "bold-34".into(),
        row,
        5,
        "\x1b[1;34;7m",
        "bold34",
        4,
    );
    push_cell!(
        &mut bytes,
        &mut map,
        "bright-94".into(),
        row,
        6,
        "\x1b[94;7m",
        "palette-fg",
        12,
    );
    push_cell!(
        &mut bytes,
        &mut map,
        "bold-bright-194".into(),
        row,
        7,
        "\x1b[1;94;7m",
        "palette-fg",
        12,
    );
    push_cell!(
        &mut bytes,
        &mut map,
        "indexed-4".into(),
        row,
        8,
        "\x1b[38;5;4;7m",
        "palette-fg",
        4,
    );
    push_cell!(
        &mut bytes,
        &mut map,
        "indexed-12".into(),
        row,
        9,
        "\x1b[38;5;12;7m",
        "palette-fg",
        12,
    );
    for (col, index) in [(10u32, 16u32), (11, 231), (12, 232), (13, 255)] {
        push_cell!(
            &mut bytes,
            &mut map,
            format!("extended-{index}"),
            row,
            col,
            &format!("\x1b[38;5;{index};7m"),
            "extended",
            index,
        );
    }
    push_cell!(
        &mut bytes,
        &mut map,
        "true-bg".into(),
        row,
        14,
        "\x1b[48;2;17;34;51m",
        "true-bg",
        0,
    );
    push_cell!(
        &mut bytes,
        &mut map,
        "true-fg".into(),
        row,
        15,
        "\x1b[38;2;68;85;102;7m",
        "true-fg",
        0,
    );
    bytes.extend_from_slice(b"\r\n");

    // A human-readable compatibility row for the owner complaint. It is not a
    // pixel sample; it keeps normal/bright blue text beside the exact swatches
    // in the retained screenshots.
    bytes.extend_from_slice(b"\x1b[34m normal 34 \x1b[0m  \x1b[94m bright 94 \x1b[0m\r\n");
    map.push(Cell {
        label: "readable-normal-34".into(),
        row: 7,
        col: 0,
        kind: "readable",
        index: 0,
    });
    map.push(Cell {
        label: "readable-bright-94".into(),
        row: 7,
        col: 1,
        kind: "readable",
        index: 0,
    });

    (bytes, map)
}

fn map_text(map: &[Cell]) -> String {
    let mut text = String::new();
    for cell in map {
        text.push_str(&format!(
            "{} {} {} {} {}\n",
            cell.label, cell.row, cell.col, cell.kind, cell.index
        ));
    }
    text
}

fn required(name: &str) -> Result<PathBuf, String> {
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| format!("{name} must be an absolute path"))
}

fn main() {
    if let Err(error) = run() {
        eprintln!("terminal-colour-fixture: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let redraw = env::args().nth(1).as_deref() == Some("redraw");
    let (bytes, map) = grid();

    let mut stdout = io::stdout();
    if redraw {
        stdout
            .write_all(&bytes)
            .map_err(|error| error.to_string())?;
        stdout.flush().map_err(|error| error.to_string())?;
        return Ok(());
    }

    let bytes_path = required("AMONG_COLOUR_FIXTURE_BYTES")?;
    let map_path = required("AMONG_COLOUR_FIXTURE_MAP")?;
    let log_path = required("AMONG_COLOUR_FIXTURE_LOG")?;
    if bytes_path.parent().is_none() || map_path.parent().is_none() || log_path.parent().is_none() {
        return Err("fixture output paths need parents".into());
    }
    fs::write(&bytes_path, &bytes).map_err(|error| error.to_string())?;
    fs::write(&map_path, map_text(&map)).map_err(|error| error.to_string())?;
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| error.to_string())?;
    writeln!(log, "pid={} start_ns={started}", std::process::id())
        .map_err(|error| error.to_string())?;
    log.flush().map_err(|error| error.to_string())?;

    stdout
        .write_all(&bytes)
        .map_err(|error| error.to_string())?;
    stdout.flush().map_err(|error| error.to_string())?;

    let error = Command::new("/bin/bash")
        .args(["--noprofile", "--norc"])
        .env("PS1", "")
        .env("PROMPT_COMMAND", "")
        .exec();
    Err(format!("could not exec bash: {error}"))
}
