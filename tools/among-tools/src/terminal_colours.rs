//! Acceptance for terminal text/background and ANSI palette choices.
//!
//! This is deliberately a thin orchestration layer over the existing owned
//! display harness. It launches the actual executable, puts the Rust colour
//! fixture in `SHELL`, drives the real Preferences controls with keys/pointer
//! events, captures the real window, and reads pixels from an ImageMagick
//! `rgba:` conversion. It never sets a colour through a test-only API.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use crate::{
    preferences, scratch,
    sessions::{self, Harness},
    storage, Result,
};
use serde_json::{json, Value};

use crate::pixels::{self, Bounds, RgbaImage};

mod source;

const APP_TITLE: &str = "Among Terminal";
const PREFS_TITLE: &str = "Preferences";
const TEXT_CONTROL: &str = "terminal-text-background";
const PALETTE_CONTROL: &str = "terminal-ansi-palette";
const BLOCK_COLOUR: [u8; 4] = [1, 2, 3, 255];
const ORACLE_SHA256: &str = "4398580c0c22b79a5cafc8d53b18a614db877496e51d5d6ce084f31b59f00762";
const ORACLE_VERSION: &str = "3.56.2";
const ORACLE_COMMIT: &str = "941d51d151c0e25b285ffe98ad4a38b4ce5dda69";

const PHASES: [(&str, &str); 11] = [
    (
        "vertical-slice",
        "Black on white + Tango through the real controls",
    ),
    (
        "owner-baseline",
        "Gray on black + Tango screenshot, ANSI 4/12 and observed contrast",
    ),
    (
        "catalogue",
        "all 11 text/background schemes and all 6 ANSI palettes",
    ),
    (
        "source",
        "private synthetic GNOME source, partial and invalid cases",
    ),
    (
        "solarized-unavailable",
        "full palette tuple equals a fresh compiled-palette baseline",
    ),
    (
        "continuity",
        "live typing, PID, selection, scrollback and split/hidden panes",
    ),
    (
        "restart",
        "fresh restore colours, with a new process identity",
    ),
    (
        "no-write",
        "opening Preferences creates no terminal.json and changes no source",
    ),
    (
        "layout",
        "toolbar minimum/allocated and five tabs in light/dark/high contrast",
    ),
    ("osc", "observed VTE OSC precedence without terminal reset"),
    (
        "bold",
        "bold-to-bright relation is unchanged by a palette/preset edit",
    ),
];

const OWN_PHASES: [&str; 4] = ["continuity", "osc", "bold", "layout"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boundary {
    Installed,
    SourceBuild,
}

impl Boundary {
    fn label(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::SourceBuild => "source-build",
        }
    }
}

struct Options {
    boundary: Boundary,
    binary: PathBuf,
    fixture: PathBuf,
    oracle: PathBuf,
    schemas: Option<PathBuf>,
    source_sha: Option<String>,
}

/// Parse the command. The binary argument is the executable under test; it is
/// never replaced by a test executable or a Cargo path.
fn options(args: &[String]) -> Result<Options> {
    let mut boundary = None;
    let mut source_sha = None;
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        match arg.as_str() {
            "--installed" => boundary = Some(Boundary::Installed),
            "--source-build" => boundary = Some(Boundary::SourceBuild),
            "--source-sha" => {
                index += 1;
                let value = args.get(index).ok_or("--source-sha needs a value")?;
                source_sha = Some(value.clone());
            }
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown check-terminal-colours option {value}").into())
            }
            _ => positional.push(PathBuf::from(arg)),
        }
        index += 1;
    }
    if positional.len() < 3 || positional.len() > 4 {
        return Err(usage().into());
    }
    let boundary = boundary.ok_or("choose --installed or --source-build")?;
    let source_sha = source_sha.or_else(|| {
        std::env::var("AMONG_CHECK_SOURCE_SHA")
            .ok()
            .filter(|value| !value.is_empty())
    });
    let fixture = fs::canonicalize(&positional[1])
        .map_err(|error| format!("colour fixture cannot be resolved: {error}"))?;
    let oracle = fs::canonicalize(&positional[2])
        .map_err(|error| format!("GNOME colour oracle cannot be resolved: {error}"))?;
    let schemas = positional
        .get(3)
        .map(fs::canonicalize)
        .transpose()
        .map_err(|error| format!("gsettings schema directory cannot be resolved: {error}"))?;
    Ok(Options {
        boundary,
        binary: positional[0].clone(),
        fixture,
        oracle,
        schemas,
        source_sha,
    })
}

fn usage() -> &'static str {
    "usage: among-tools check-terminal-colours \
     (--installed|--source-build) [--source-sha SHA] \
     <among-executable> <terminal-colour-fixture> <gnome-colours.json> [<gsettings-schema-dir>]"
}

/// A cell in the fixture map. Only labels/positions/kinds come from the
/// fixture; expected catalog colours come from the independent JSON oracle.
#[derive(Debug, Clone)]
struct CellDef {
    label: String,
    row: u32,
    col: u32,
    kind: String,
    index: u32,
}

struct Calibration {
    image: RgbaImage,
    png: PathBuf,
    raw: PathBuf,
    terminal: [f64; 4],
    block: Bounds,
    marker_pixels: u64,
    origin_x: f64,
    origin_y: f64,
    pitch_x: f64,
    pitch_y: f64,
}

struct Oracle {
    schemes: BTreeMap<String, ([u8; 3], [u8; 3])>,
    palettes: BTreeMap<String, Vec<[u8; 3]>>,
    scheme_order: Vec<String>,
    palette_order: Vec<String>,
}

impl Oracle {
    fn read(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)?;
        if !metadata.is_file() || metadata.len() > 2 * 1024 * 1024 {
            return Err("GNOME colour oracle is not a bounded regular file".into());
        }
        let digest = storage::sha256(File::open(path)?)?;
        if digest != ORACLE_SHA256 {
            return Err(format!(
                "GNOME colour oracle SHA-256 is {digest}, expected {ORACLE_SHA256}"
            )
            .into());
        }
        let bytes = fs::read(path)?;
        let document: Value = serde_json::from_slice(&bytes)?;
        if document["source"]["version"].as_str() != Some(ORACLE_VERSION)
            || document["source"]["commit"].as_str() != Some(ORACLE_COMMIT)
        {
            return Err("GNOME colour oracle version/commit pin differs".into());
        }
        let mut schemes = BTreeMap::new();
        let mut scheme_order = Vec::new();
        for entry in document["text_background"]
            .as_array()
            .ok_or("oracle has no text_background list")?
        {
            let id = entry["id"].as_str().ok_or("oracle scheme lacks id")?;
            if schemes.contains_key(id) {
                return Err(format!("oracle scheme {id} is duplicated").into());
            }
            let fg = rgb(entry["foreground"]
                .as_array()
                .ok_or("scheme lacks foreground")?)?;
            let bg = rgb(entry["background"]
                .as_array()
                .ok_or("scheme lacks background")?)?;
            schemes.insert(id.to_string(), (fg, bg));
            scheme_order.push(id.to_string());
        }
        let mut palettes = BTreeMap::new();
        let mut palette_order = Vec::new();
        for entry in document["ansi_palettes"]
            .as_array()
            .ok_or("oracle has no ansi_palettes list")?
        {
            let id = entry["id"].as_str().ok_or("oracle palette lacks id")?;
            if palettes.contains_key(id) {
                return Err(format!("oracle palette {id} is duplicated").into());
            }
            let colours = entry["colours"]
                .as_array()
                .ok_or("oracle palette lacks colours")?
                .iter()
                .map(|value| -> Result<[u8; 3]> {
                    let channels = value
                        .as_array()
                        .ok_or("oracle palette entry is not an array")?;
                    rgb(channels)
                })
                .collect::<Result<Vec<_>>>()?;
            if colours.len() != 16 {
                return Err(format!("oracle palette {id} does not have 16 entries").into());
            }
            palettes.insert(id.to_string(), colours);
            palette_order.push(id.to_string());
        }
        if schemes.len() != 11 || palettes.len() != 6 {
            return Err("oracle does not contain the pinned 11+6 catalogue".into());
        }
        Ok(Self {
            schemes,
            palettes,
            scheme_order,
            palette_order,
        })
    }

    fn scheme(&self, id: &str) -> Result<([u8; 3], [u8; 3])> {
        self.schemes
            .get(id)
            .copied()
            .ok_or_else(|| format!("oracle has no scheme {id}").into())
    }

    fn palette(&self, id: &str) -> Result<&[[u8; 3]]> {
        self.palettes
            .get(id)
            .map(Vec::as_slice)
            .ok_or_else(|| format!("oracle has no palette {id}").into())
    }
}

fn rgb(channels: &[Value]) -> Result<[u8; 3]> {
    if channels.len() != 3 {
        return Err("oracle RGB value does not have three channels".into());
    }
    let channel = |index: usize| -> Result<u8> {
        u8::try_from(
            channels[index]
                .as_u64()
                .ok_or("oracle RGB channel is not unsigned")?,
        )
        .map_err(|_| "oracle RGB channel exceeds u8".into())
    };
    Ok([channel(0)?, channel(1)?, channel(2)?])
}

fn read_map(path: &Path) -> Result<Vec<CellDef>> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err("fixture map is not a bounded regular file".into());
    }
    let text = fs::read_to_string(path)?;
    let mut cells = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err("fixture map line is malformed".into());
        }
        let row = fields[1].parse::<u32>()?;
        let col = fields[2].parse::<u32>()?;
        let index = fields[4].parse::<u32>()?;
        cells.push(CellDef {
            label: fields[0].to_string(),
            row,
            col,
            kind: fields[3].to_string(),
            index,
        });
    }
    if cells.is_empty() {
        return Err("fixture map is empty".into());
    }
    Ok(cells)
}

/// The fixture map is data, not an assertion. Validate its complete fixed shape
/// before any sample so a renamed/duplicated/mis-indexed cell is a setup error.
fn validate_map(cells: &[CellDef]) -> Result<()> {
    let mut labels = BTreeMap::new();
    let mut positions = BTreeMap::new();
    for cell in cells {
        if labels.insert(cell.label.clone(), cell).is_some() {
            return Err(format!("fixture map duplicates label {}", cell.label).into());
        }
        if !matches!(
            cell.kind.as_str(),
            "block"
                | "default-bg"
                | "default-fg"
                | "palette-bg"
                | "palette-fg"
                | "bold34"
                | "extended"
                | "true-bg"
                | "true-fg"
                | "readable"
        ) {
            return Err(format!("fixture map has unknown kind {}", cell.kind).into());
        }
        if positions
            .insert((cell.row, cell.col), &cell.label)
            .is_some()
        {
            return Err(format!(
                "fixture map duplicates grid position {},{}",
                cell.row, cell.col
            )
            .into());
        }
        let valid_index = match cell.kind.as_str() {
            "palette-bg" | "palette-fg" => cell.index < 16,
            "extended" => matches!(cell.index, 16 | 231 | 232 | 255),
            "bold34" => cell.index == 4,
            "block" => cell.index == 4,
            _ => cell.index == 0,
        };
        if !valid_index {
            return Err(format!(
                "fixture map {} has invalid index {} for kind {}",
                cell.label, cell.index, cell.kind
            )
            .into());
        }
        // The label names the palette or extended index the cell stands for,
        // so the recorded index must be that same number.
        let named_index = ["ansi-bg-", "ansi-fg-"]
            .iter()
            .find_map(|prefix| cell.label.strip_prefix(prefix))
            .and_then(|digits| digits.parse::<u32>().ok())
            .or_else(|| {
                cell.label
                    .strip_prefix("extended-")
                    .and_then(|digits| digits.parse::<u32>().ok())
            });
        if named_index.is_some_and(|named| named != cell.index) {
            return Err(format!(
                "fixture map {} records index {} instead of the index in its label",
                cell.label, cell.index
            )
            .into());
        }
        let expected = expected_position(&cell.label);
        if expected != Some((cell.row, cell.col)) {
            return Err(format!(
                "fixture map {} is at {},{} instead of {:?}",
                cell.label, cell.row, cell.col, expected
            )
            .into());
        }
        if cell.kind != "block" && (cell.row < 4 && cell.col < 4) {
            return Err(
                format!("fixture map {} overlaps the calibration block", cell.label).into(),
            );
        }
    }
    for label in required_labels() {
        if !labels.contains_key(&label) {
            return Err(format!("fixture map lacks {label}").into());
        }
    }
    if labels.len() != required_labels().len() {
        return Err("fixture map contains an unplanned cell".into());
    }
    Ok(())
}

fn expected_position(label: &str) -> Option<(u32, u32)> {
    if label == "calibration-block" {
        return Some((0, 0));
    }
    if let Some(index) = label.strip_prefix("ansi-bg-") {
        return Some((4, index.parse::<u32>().ok()?));
    }
    if let Some(index) = label.strip_prefix("ansi-fg-") {
        return Some((5, index.parse::<u32>().ok()?));
    }
    let col = match label {
        "default-bg" => 0,
        "default-fg" => 1,
        "reset-bg" => 2,
        "reset-fg" => 3,
        "normal-34" => 4,
        "bold-34" => 5,
        "bright-94" => 6,
        "bold-bright-194" => 7,
        "indexed-4" => 8,
        "indexed-12" => 9,
        "extended-16" => 10,
        "extended-231" => 11,
        "extended-232" => 12,
        "extended-255" => 13,
        "true-bg" => 14,
        "true-fg" => 15,
        "readable-normal-34" => return Some((7, 0)),
        "readable-bright-94" => return Some((7, 1)),
        _ => return None,
    };
    Some((6, col))
}

fn required_labels() -> Vec<String> {
    let mut labels = vec![
        "calibration-block",
        "default-bg",
        "default-fg",
        "reset-bg",
        "reset-fg",
        "normal-34",
        "bold-34",
        "bright-94",
        "bold-bright-194",
        "indexed-4",
        "indexed-12",
        "extended-16",
        "extended-231",
        "extended-232",
        "extended-255",
        "true-bg",
        "true-fg",
        "readable-normal-34",
        "readable-bright-94",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    for index in 0..16 {
        labels.push(format!("ansi-bg-{index}"));
        labels.push(format!("ansi-fg-{index}"));
    }
    labels
}

fn latest_field<'a>(text: &'a str, field: &str) -> Option<&'a str> {
    let needle = format!(" {field}=");
    text.lines()
        .rev()
        .find_map(|line| line.split_once(&needle).map(|(_, value)| value))
}

pub(super) fn rectangle_entries<'a>(text: &'a str, field_name: &str) -> Result<&'a str> {
    let tail = latest_field(text, field_name)
        .ok_or_else(|| format!("latest selfcheck lacks {field_name}"))?;
    let tail = tail.trim_start();
    let opening = tail
        .strip_prefix('[')
        .ok_or_else(|| format!("latest {field_name} has no opening bracket"))?;
    let closing = opening
        .find(']')
        .ok_or_else(|| format!("latest {field_name} has no closing bracket"))?;
    Ok(&opening[..closing])
}

fn rect_of(text: &str, field_name: &str, name: &str) -> Result<[f64; 4]> {
    let entries = rectangle_entries(text, field_name)?;
    if entries.trim().is_empty() {
        return Err(format!("latest {field_name} is empty").into());
    }
    sessions::rectangles(entries, name)
        .first()
        .copied()
        .ok_or_else(|| format!("latest {field_name} lacks {name}").into())
}

fn wait_terminal_rects(h: &mut Harness) -> Result<()> {
    // The owned fixture's calibrated cell pitch is already larger than this;
    // 12px excludes the 2x2 pre-layout rectangle without weakening a real
    // terminal allocation.
    const MIN_SIDE: f64 = 12.0;
    h.wait(20, |this| {
        let Ok(rect) = rect_of(&this.text()?, "terminal_rects", "1") else {
            return Ok(false);
        };
        Ok(rect[2] >= MIN_SIDE && rect[3] >= MIN_SIDE)
    })
}

/// Parse the Debug-printed `prefs_terminal` tuples. The app prints
/// `("name", "id", "subtitle")`; Debug escaping is respected and no CSV field
/// with a subtitle is split by whitespace.
fn prefs_terminal_map(text: &str) -> Result<BTreeMap<String, (String, String)>> {
    let tail =
        latest_field(text, "prefs_terminal").ok_or("selfcheck lacks latest prefs_terminal")?;
    let strings = quoted_strings(tail)?;
    if strings.len() % 3 != 0 {
        return Err("latest prefs_terminal tuple list is incomplete".into());
    }
    let mut map = BTreeMap::new();
    for tuple in strings.as_chunks::<3>().0 {
        map.insert(tuple[0].clone(), (tuple[1].clone(), tuple[2].clone()));
    }
    Ok(map)
}

fn quoted_strings(text: &str) -> Result<Vec<String>> {
    let mut strings = Vec::new();
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        if character != '"' {
            continue;
        }
        let mut value = String::new();
        loop {
            let character = chars.next().ok_or("unterminated debug string")?;
            match character {
                '"' => break,
                '\\' => {
                    let escaped = chars.next().ok_or("unterminated debug escape")?;
                    value.push(match escaped {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        '"' => '"',
                        '\\' => '\\',
                        other => other,
                    });
                }
                other => value.push(other),
            }
        }
        strings.push(value);
    }
    Ok(strings)
}

#[derive(Debug, Clone)]
struct TabObservation {
    label: String,
    width: i32,
    text_width: i32,
    label_alloc: i32,
    parent_width: i32,
    ellipsized: bool,
    mapped: bool,
}

/// The rectangles and the tab list are two lines of one status tick. A read
/// between them would pair this tick's rectangles with an earlier, possibly
/// empty tab list, so require both newest lines to come from the same tick and
/// the tab list to be non-empty.
fn tab_snapshot_is_current(text: &str) -> bool {
    let tick = |field: &str| {
        let needle = format!(" {field}=");
        text.lines()
            .rev()
            .find(|line| line.contains(&needle))
            .and_then(|line| line.split_once("]:").map(|(label, _)| label.to_owned()))
    };
    match (tick("prefs_rects"), tick("prefs_tabs")) {
        (Some(rects), Some(tabs)) if rects == tabs => {
            prefs_tabs(text).is_ok_and(|tabs| !tabs.is_empty())
        }
        _ => false,
    }
}

fn prefs_tabs(text: &str) -> Result<Vec<TabObservation>> {
    let raw = latest_field(text, "prefs_tabs")
        .ok_or("latest selfcheck lacks prefs_tabs from the app observer")?;
    let document: Value = serde_json::from_str(raw)?;
    let entries = document
        .as_array()
        .ok_or("prefs_tabs is not a JSON array")?;
    let mut tabs = Vec::new();
    for entry in entries {
        let fields = entry
            .as_array()
            .ok_or("prefs_tabs entry is not a JSON array")?;
        if fields.len() != 12 {
            return Err("prefs_tabs entry does not have 12 fields".into());
        }
        let index = fields[0].as_u64().ok_or("tab index is not unsigned")?;
        if index != tabs.len() as u64 {
            return Err("prefs_tabs entries are not in index order".into());
        }
        let label = fields[1].as_str().ok_or("tab label is not a string")?;
        for position in [2usize, 3, 4, 5, 6, 7, 8, 9] {
            fields[position]
                .as_i64()
                .ok_or("tab geometry/measurement is not signed")?;
        }
        let width = fields[4].as_i64().ok_or("tab width is not signed")?;
        let text_width = fields[6].as_i64().ok_or("tab text width is not signed")?;
        let label_alloc = fields[7]
            .as_i64()
            .ok_or("tab label allocation is not signed")?;
        let parent_width = fields[8].as_i64().ok_or("tab parent width is not signed")?;
        let ellipsized = fields[10]
            .as_bool()
            .ok_or("tab ellipsized flag is not bool")?;
        let mapped = fields[11].as_bool().ok_or("tab mapped flag is not bool")?;
        tabs.push(TabObservation {
            label: label.to_string(),
            width: i32::try_from(width).map_err(|_| "tab width exceeds i32".to_string())?,
            text_width: i32::try_from(text_width)
                .map_err(|_| "tab text width exceeds i32".to_string())?,
            label_alloc: i32::try_from(label_alloc)
                .map_err(|_| "tab label allocation exceeds i32".to_string())?,
            parent_width: i32::try_from(parent_width)
                .map_err(|_| "tab parent width exceeds i32".to_string())?,
            ellipsized,
            mapped,
        });
    }
    Ok(tabs)
}

fn validate_tabs(tabs: &[TabObservation]) -> Result<()> {
    const LABELS: [&str; 5] = ["Plugins", "Notifications", "Keyboard", "Terminal", "About"];
    if tabs.len() != LABELS.len() {
        return Err(format!("expected five mapped tabs, got {}", tabs.len()).into());
    }
    let widths = tabs.iter().map(|tab| tab.width).collect::<Vec<_>>();
    let minimum = widths.iter().copied().min().unwrap_or(0);
    let maximum = widths.iter().copied().max().unwrap_or(0);
    if minimum <= 0 || maximum - minimum > 1 {
        return Err(format!("tab widths are not homogeneous: {widths:?}").into());
    }
    for (tab, expected) in tabs.iter().zip(LABELS) {
        if tab.label != expected {
            return Err(format!("expected tab {expected:?}, got {:?}", tab.label).into());
        }
        if !tab.mapped || tab.width <= 0 {
            return Err(format!("tab {expected:?} is not mapped with allocation").into());
        }
        if tab.ellipsized {
            return Err(format!("tab {expected:?} is ellipsized").into());
        }
        if tab.text_width <= 0
            || tab.text_width > tab.label_alloc
            || tab.label_alloc > tab.parent_width
        {
            return Err(format!(
                "tab {expected:?} full text does not fit: text {} allocation {} parent {}",
                tab.text_width, tab.label_alloc, tab.parent_width
            )
            .into());
        }
    }
    Ok(())
}
fn terminal_json(h: &Harness) -> Result<Option<Value>> {
    let path = h.base.join("config/among/terminal.json");
    match fs::metadata(&path) {
        Ok(metadata) if metadata.is_file() && metadata.len() <= 64 * 1024 => {}
        Ok(_) => return Err("terminal.json is not a bounded regular file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let bytes = fs::read(&path)?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn saved_appearance_matches(document: &Value, key: &str, expected: &str) -> Result<bool> {
    let object = document
        .as_object()
        .ok_or("terminal.json is not a JSON object")?;
    match object.get(key) {
        None => Ok(expected == "gnome-terminal-default"),
        Some(Value::String(value)) => Ok(value == expected),
        Some(Value::Null) => Err(format!("terminal.json {key} is null, not absent").into()),
        Some(_) => Err(format!("terminal.json {key} has the wrong type").into()),
    }
}

fn wait_selection(h: &mut Harness, text_id: &str, palette_id: &str) -> Result<()> {
    h.wait(10, |this| {
        let text = this.text()?;
        let Ok(terminal) = prefs_terminal_map(&text) else {
            return Ok(false);
        };
        let text_ok = terminal
            .get(TEXT_CONTROL)
            .is_some_and(|(id, _)| id == text_id);
        let palette_ok = terminal
            .get(PALETTE_CONTROL)
            .is_some_and(|(id, _)| id == palette_id);
        let Some(document) = terminal_json(this)? else {
            return Ok(false);
        };
        if !text_ok || !palette_ok {
            return Ok(false);
        }
        Ok(
            saved_appearance_matches(&document, "text_background", text_id)?
                && saved_appearance_matches(&document, "ansi_palette", palette_id)?,
        )
    })
}

fn window_ids(h: &Harness, pid: u32) -> Result<Vec<u32>> {
    let pid = pid.to_string();
    let output = match h.probe(
        "/usr/bin/xdotool",
        &["search", "--onlyvisible", "--pid", &pid],
    ) {
        Ok(output) => output,
        Err(_) => return Ok(Vec::new()),
    };
    Ok(output
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .collect())
}

fn find_window(h: &Harness, pid: u32, title: &str) -> Result<Option<u32>> {
    for id in window_ids(h, pid)? {
        let id_text = id.to_string();
        if h.probe("/usr/bin/xdotool", &["getwindowname", &id_text])
            .is_ok_and(|name| name.trim() == title)
        {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

fn wait_window(h: &mut Harness, pid: u32, title: &str, seconds: u64) -> Result<u32> {
    h.wait(seconds, |this| Ok(find_window(this, pid, title)?.is_some()))?;
    find_window(h, pid, title)?.ok_or_else(|| format!("{title} window disappeared").into())
}

fn verify_executed(options: &Options, pid: u32) -> Result<(String, String)> {
    let input = fs::canonicalize(&options.binary)
        .map_err(|error| format!("input executable cannot be canonicalised: {error}"))?;
    let link = fs::read_link(format!("/proc/{pid}/exe"))
        .map_err(|error| format!("executed executable is not readable: {error}"))?;
    if link.to_string_lossy().ends_with(" (deleted)") {
        return Err("executed executable was deleted after launch".into());
    }
    let executed = fs::canonicalize(&link)
        .map_err(|error| format!("executed executable cannot be canonicalised: {error}"))?;
    if input != executed {
        return Err(format!(
            "executed file {} differs from input {}",
            executed.display(),
            input.display()
        )
        .into());
    }
    let input_sha = storage::sha256(File::open(&options.binary)?)?;
    let executed_sha = storage::sha256(File::open(&executed)?)?;
    if input_sha != executed_sha {
        return Err(format!(
            "executed SHA-256 {executed_sha} differs from input SHA-256 {input_sha}"
        )
        .into());
    }
    Ok((executed.display().to_string(), executed_sha))
}

fn capture_window(h: &Harness, window: u32, path: &Path) -> Result<()> {
    h.probe(
        "/usr/bin/import",
        &[
            "-window",
            &window.to_string(),
            path.to_str().ok_or("capture path is not UTF-8")?,
        ],
    )?;
    Ok(())
}

fn window_size(h: &Harness, window: u32) -> Result<(u32, u32)> {
    let text = h.probe(
        "/usr/bin/xdotool",
        &["getwindowgeometry", "--shell", &window.to_string()],
    )?;
    let value = |name: &str| -> Result<u32> {
        text.lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .and_then(|value| value.trim().parse::<u32>().ok())
            .ok_or_else(|| format!("xdotool did not report {name}").into())
    };
    Ok((value("WIDTH")?, value("HEIGHT")?))
}

fn probe_rgba_order(h: &Harness, base: &Path) -> Result<()> {
    let path = base.join("rgba-probe.raw");
    h.probe(
        "/usr/bin/convert",
        &[
            "-size",
            "1x1",
            "xc:rgba(10,20,30,0.5)",
            "-depth",
            "8",
            &format!("rgba:{}", path.display()),
        ],
    )?;
    let metadata = fs::metadata(&path)?;
    if metadata.len() != 4 {
        return Err("ImageMagick rgba: probe did not produce four bytes".into());
    }
    let bytes = fs::read(&path)?;
    if bytes.len() != 4
        || bytes[0] != 10
        || bytes[1] != 20
        || bytes[2] != 30
        || !(127..=128).contains(&bytes[3])
    {
        return Err(format!(
            "ImageMagick rgba: probe returned {bytes:?}, expected straight [10,20,30,127..128]"
        )
        .into());
    }
    Ok(())
}

fn decode_capture(h: &Harness, window: u32, png: &Path, raw: &Path) -> Result<RgbaImage> {
    let size = h.probe(
        "/usr/bin/identify",
        &[
            "-format",
            "%w %h",
            png.to_str().ok_or("PNG path is not UTF-8")?,
        ],
    )?;
    let (width, height) = {
        let mut parts = size.split_whitespace();
        let width = parts
            .next()
            .ok_or("identify did not report width")?
            .parse::<u32>()?;
        let height = parts
            .next()
            .ok_or("identify did not report height")?
            .parse::<u32>()?;
        (width, height)
    };
    let (window_width, window_height) = window_size(h, window)?;
    if (width, height) != (window_width, window_height) {
        return Err(format!(
            "PNG {width}x{height} differs from X window {window_width}x{window_height}; \
             CSD/shadow mapping is not silently corrected"
        )
        .into());
    }
    h.probe(
        "/usr/bin/convert",
        &[
            png.to_str().ok_or("PNG path is not UTF-8")?,
            "-depth",
            "8",
            &format!("rgba:{}", raw.display()),
        ],
    )?;
    RgbaImage::read_raw(raw, width, height)
}

fn calibrate(image: RgbaImage, png: &Path, raw: &Path, terminal: [f64; 4]) -> Result<Calibration> {
    let bounds = Bounds {
        x: terminal[0].max(0.0) as u32,
        y: terminal[1].max(0.0) as u32,
        width: terminal[2].max(0.0) as u32,
        height: terminal[3].max(0.0) as u32,
    };
    let block = image
        .largest_solid_component(bounds, BLOCK_COLOUR)
        .ok_or("calibration block filled component was not found in the captured window")?;
    let size = 4.0f64;
    let pitch_x = f64::from(block.width) / size;
    let pitch_y = f64::from(block.height) / size;
    if pitch_x < 3.0 || pitch_y < 3.0 {
        return Err(format!(
            "calibration block produced implausible cell pitch {pitch_x}x{pitch_y}"
        )
        .into());
    }
    let marker_pixels = u64::from(block.width) * u64::from(block.height);
    let sample = image.sample_solid(
        block.x + (pitch_x / 2.0) as u32,
        block.y + (pitch_y / 2.0) as u32,
        3,
        0,
    )?;
    if sample.rgba != BLOCK_COLOUR {
        return Err(format!(
            "live calibration patch measured {:?}, expected {BLOCK_COLOUR:?}",
            sample.rgba
        )
        .into());
    }
    Ok(Calibration {
        image,
        png: png.to_path_buf(),
        raw: raw.to_path_buf(),
        terminal,
        block,
        marker_pixels,
        origin_x: f64::from(block.x),
        origin_y: f64::from(block.y),
        pitch_x,
        pitch_y,
    })
}

fn capture_calibrated(
    h: &mut Harness,
    app_window: u32,
    tag: &str,
    report: &mut Value,
) -> Result<(Calibration, Vec<CellDef>)> {
    let png = h.base.join(format!("{tag}.png"));
    let raw = h.base.join(format!("{tag}.rgba"));
    h.screenshot(&format!("{tag}.png"))?;
    append_capture(report, &png)?;
    let image = decode_capture(h, app_window, &png, &raw)?;
    let terminal = rect_of(&h.text()?, "terminal_rects", "1")?;
    let calibration = calibrate(image, &png, &raw, terminal)?;
    append_calibration(report, tag, &calibration)?;
    let cells = read_map(&h.base.join("fixture-map.txt"))?;
    validate_map(&cells)?;
    Ok((calibration, cells))
}

fn sample_cell(calibration: &Calibration, cell: &CellDef) -> Result<pixels::Sample> {
    let x = calibration.origin_x + (f64::from(cell.col) + 0.5) * calibration.pitch_x;
    let y = calibration.origin_y + (f64::from(cell.row) + 0.5) * calibration.pitch_y;
    if x < 1.0 || y < 1.0 {
        return Err("cell centre leaves the capture".into());
    }
    calibration
        .image
        .sample_solid(x.round() as u32, y.round() as u32, 3, 1)
}

fn expected_for(cell: &CellDef, pair: ([u8; 3], [u8; 3]), palette: &[[u8; 3]]) -> Option<[u8; 3]> {
    match cell.kind.as_str() {
        "default-bg" => Some(pair.1),
        "default-fg" => Some(pair.0),
        "palette-bg" | "palette-fg" => palette.get(cell.index as usize).copied(),
        "true-bg" => Some([17, 34, 51]),
        "true-fg" => Some([68, 85, 102]),
        "extended" => expected_extended(cell.index),
        _ => None,
    }
}

/// VTE's compiled xterm-compatible extended entries at the representative
/// indices used by the fixture. The selected 16-entry preset must not alter
/// these entries.
fn expected_extended(index: u32) -> Option<[u8; 3]> {
    match index {
        16 => Some([0, 0, 0]),
        231 => Some([255, 255, 255]),
        232 => Some([8, 8, 8]),
        255 => Some([238, 238, 238]),
        _ => None,
    }
}

fn sample_cells(
    calibration: &Calibration,
    cells: &[CellDef],
    keep: impl Fn(&CellDef) -> bool,
    pair: ([u8; 3], [u8; 3]),
    palette: &[[u8; 3]],
) -> Result<(Vec<Value>, Vec<String>)> {
    let mut samples = Vec::new();
    let mut failures = Vec::new();
    for cell in cells.iter().filter(|cell| keep(cell)) {
        let sample = sample_cell(calibration, cell)?;
        let expected = expected_for(cell, pair, palette);
        let mut record = json!({
            "label": cell.label,
            "row": cell.row,
            "col": cell.col,
            "kind": cell.kind,
            "x": (calibration.origin_x + (f64::from(cell.col) + 0.5) * calibration.pitch_x).round() as u32,
            "y": (calibration.origin_y + (f64::from(cell.row) + 0.5) * calibration.pitch_y).round() as u32,
            "patch": [3, 3],
            "rgba": sample.rgba,
            "max_delta": sample.max_delta,
        });
        if let Some(expected) = expected {
            record["expected_rgb"] = json!(expected);
            record["expected_hex"] = json!(pixels::hex(expected));
            if !pixels::close_rgb(sample.rgba, expected, 1) {
                failures.push(format!(
                    "{} measured {:?}, expected {}",
                    cell.label,
                    sample.rgba,
                    pixels::hex(expected)
                ));
            }
        } else {
            record["expected_rgb"] = Value::Null;
            record["verification"] = json!("unverified");
            record["reason"] = json!(match cell.kind.as_str() {
                "bold34" => "bold policy is checked by the separate bold phase",
                "extended" => "extended VTE compiled palette is checked by its own phase",
                _ => "no oracle expectation for this cell kind",
            });
        }
        samples.push(record);
    }
    Ok((samples, failures))
}

fn append_calibration(report: &mut Value, tag: &str, calibration: &Calibration) -> Result<()> {
    report["calibrations"]
        .as_array_mut()
        .ok_or("report calibrations is not an array")?
        .push(json!({
            "capture": tag,
            "png": calibration.png.display().to_string(),
            "raw": calibration.raw.display().to_string(),
            "png_sha256": storage::sha256(File::open(&calibration.png)?)?,
            "raw_sha256": storage::sha256(File::open(&calibration.raw)?)?,
            "terminal": calibration.terminal,
            "block": [calibration.block.x, calibration.block.y, calibration.block.width, calibration.block.height],
            "marker_pixels": calibration.marker_pixels,
            "cell_pitch": [calibration.pitch_x, calibration.pitch_y],
        }));
    Ok(())
}

fn append_samples(report: &mut Value, tag: &str, mut samples: Vec<Value>) -> Result<()> {
    for sample in &mut samples {
        sample["capture"] = json!(tag);
    }
    report["samples"]
        .as_array_mut()
        .ok_or("report samples is not an array")?
        .extend(samples);
    Ok(())
}

fn relative_luminance(rgb: [u8; 3]) -> f64 {
    let channel = |value: u8| {
        let value = f64::from(value) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(rgb[0]) + 0.7152 * channel(rgb[1]) + 0.0722 * channel(rgb[2])
}

fn contrast_ratio(foreground: [u8; 3], background: [u8; 3]) -> f64 {
    let first = relative_luminance(foreground);
    let second = relative_luminance(background);
    let lighter = first.max(second);
    let darker = first.min(second);
    (lighter + 0.05) / (darker + 0.05)
}

fn latest_scalar(text: &str, field: &str) -> Option<f64> {
    latest_field(text, field)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn sample_rgb(samples: &[Value], label: &str) -> Result<[u8; 3]> {
    let rgba = samples
        .iter()
        .find(|sample| sample["label"].as_str() == Some(label))
        .and_then(|sample| sample["rgba"].as_array())
        .ok_or_else(|| format!("samples lack {label}"))?;
    let channel = |index: usize| -> Result<u8> {
        u8::try_from(
            rgba.get(index)
                .and_then(Value::as_u64)
                .ok_or("sample channel is not unsigned")?,
        )
        .map_err(|_| "sample channel exceeds u8".into())
    };
    Ok([channel(0)?, channel(1)?, channel(2)?])
}

fn sample_one(calibration: &Calibration, cells: &[CellDef], label: &str) -> Result<pixels::Sample> {
    let cell = cells
        .iter()
        .find(|cell| cell.label == label)
        .ok_or_else(|| format!("fixture map lacks {label}"))?;
    sample_cell(calibration, cell)
}

fn proc_start_time(pid: u32) -> Result<u64> {
    let path = format!("/proc/{pid}/stat");
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file() || metadata.len() > 8 * 1024 {
        return Err("process stat is not a bounded regular file".into());
    }
    let stat = fs::read_to_string(path)?;
    let after_name = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest)
        .ok_or("process stat has no command terminator")?;
    after_name
        .split_whitespace()
        .nth(19)
        .ok_or("process stat has no starttime")?
        .parse::<u64>()
        .map_err(|_| "process starttime is not unsigned".into())
}

fn proc_alive(pid: u32, start_time: u64) -> bool {
    proc_start_time(pid).is_ok_and(|current| current == start_time)
}

fn latest_workspaces(text: &str) -> Option<u32> {
    latest_field(text, "workspaces")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn latest_active(text: &str) -> Option<u32> {
    latest_field(text, "active")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn latest_panes(text: &str) -> Option<u32> {
    latest_field(text, "panes")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn latest_selection(text: &str) -> Option<String> {
    latest_field(text, "terminal_selections").map(str::to_string)
}

fn latest_field_after<'a>(text: &'a str, offset: usize, field: &str) -> Option<&'a str> {
    latest_field(text.get(offset..)?, field)
}

fn pane_scroll_snapshot_value(value: &str) -> Option<(f64, i32)> {
    let entry = value
        .split('"')
        .find(|entry| entry.starts_with("1:"))?
        .strip_prefix("1:")?;
    let (lines, cell) = entry.split_once('@')?;
    Some((lines.parse().ok()?, cell.trim().parse().ok()?))
}

fn fixture_pids(h: &Harness) -> Result<Vec<u32>> {
    let path = h.base.join("fixture.log");
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return Err("fixture log is not a bounded regular file".into());
    }
    let text = fs::read_to_string(path)?;
    let mut pids = Vec::new();
    for line in text.lines() {
        if let Some(pid) = line.strip_prefix("pid=").and_then(|value| {
            value
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u32>().ok())
        }) {
            if !pids.contains(&pid) {
                pids.push(pid);
            }
        }
    }
    if pids.is_empty() {
        return Err("fixture log has no PID".into());
    }
    Ok(pids)
}

fn pane_scroll_moved(text: &str) -> bool {
    pane_scroll_snapshot(text).is_some_and(|(lines, _)| lines > 0.01)
}

fn pane_scroll_snapshot(text: &str) -> Option<(f64, i32)> {
    pane_scroll_snapshot_value(latest_field(text, "pane_scroll")?)
}

fn pane_scroll_snapshot_after(text: &str, offset: usize) -> Option<(f64, i32)> {
    pane_scroll_snapshot_value(latest_field_after(text, offset, "pane_scroll")?)
}

/// The application prints `prefs_rects` only while a Preferences window
/// exists, and prints `prefs_tabs` on every status tick. Preferences is
/// therefore gone once the newest tick that reported `prefs_tabs` did not
/// also report `prefs_rects`; a tick label is the `among[tickN]` prefix.
fn preferences_report_is_gone(text: &str) -> bool {
    let label = |line: &str| line.split_once("]:").map(|(label, _)| label.to_owned());
    let Some(latest_tick) = text
        .lines()
        .rev()
        .find(|line| line.contains(" prefs_tabs="))
        .and_then(label)
    else {
        return false;
    };
    text.lines()
        .rev()
        .find(|line| line.contains(" prefs_rects="))
        .and_then(label)
        .is_none_or(|last_rects_tick| last_rects_tick != latest_tick)
}

fn open_preferences(h: &mut Harness, pid: u32, script: &Path, mark: &str) -> Result<u32> {
    let offset = h.text()?.len();
    h.key("ctrl+comma")?;
    let preferences = wait_window(h, pid, PREFS_TITLE, 20)?;
    fs::write(script.with_extension(mark), b"go")?;
    h.wait(20, |this| {
        let text = this.text()?;
        let Some(tail) = text.get(offset..) else {
            return Ok(false);
        };
        let Ok(entries) = rectangle_entries(tail, "prefs_rects") else {
            return Ok(false);
        };
        Ok(!sessions::rectangles(entries, TEXT_CONTROL).is_empty())
    })?;
    Ok(preferences)
}

fn choose_combo(h: &Harness, preferences_window: u32, control: &str, target: u32) -> Result<()> {
    let rect = rect_of(&h.text()?, "prefs_rects", control)?;
    let x = (rect[0] + rect[2] / 2.0).round() as i64;
    let y = (rect[1] + rect[3] / 2.0).round() as i64;
    h.xdo(&[
        "mousemove",
        "--window",
        &preferences_window.to_string(),
        &x.to_string(),
        &y.to_string(),
    ])?;
    h.xdo(&["click", "1"])?;
    std::thread::sleep(std::time::Duration::from_millis(200));
    h.key("Home")?;
    for _ in 0..target {
        h.key("Down")?;
    }
    h.key("Return")?;
    Ok(())
}

/// Choose a palette entry and wait for the application to report and persist
/// it. A failed wait keeps a screenshot of the Preferences window, so a combo
/// that never changed is distinguishable from one that changed but was not
/// reported.
fn select_palette(
    h: &mut Harness,
    preferences_window: u32,
    index: u32,
    text_id: &str,
    palette_id: &str,
    report: &mut Value,
) -> Result<()> {
    choose_combo(h, preferences_window, PALETTE_CONTROL, index)?;
    if let Err(error) = wait_selection(h, text_id, palette_id) {
        let png = h.base.join(format!("select-{palette_id}-failure.png"));
        let _ = capture_window(h, preferences_window, &png);
        let _ = append_capture(report, &png);
        return Err(error);
    }
    Ok(())
}

fn close_preferences(
    h: &mut Harness,
    pid: u32,
    preferences_window: u32,
    app_window: u32,
) -> Result<()> {
    let offset = h.text()?.len();
    let id = preferences_window.to_string();
    h.xdo(&["windowactivate", "--sync", &id])?;
    h.xdo(&["windowfocus", "--sync", &id])?;
    // This is the window manager's ordinary close command, not XKillClient:
    // the application receives a normal delete event and GTK does not see its
    // surface destroyed underneath a live GdkSurface.
    h.key("alt+F4")?;
    h.wait(
        10,
        |this| Ok(find_window(this, pid, PREFS_TITLE)?.is_none()),
    )?;
    h.wait(10, |this| {
        let text = this.text()?;
        Ok(text.get(offset..).is_some_and(preferences_report_is_gone))
    })?;
    if find_window(h, pid, APP_TITLE)? != Some(app_window) {
        return Err("application window disappeared while closing Preferences".into());
    }
    Ok(())
}

/// Single-quote a path for the shell, so an apostrophe in it cannot end the
/// quoting early.
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

fn append_phase(report: &mut Value, name: &str, status: &str, detail: &str) -> Result<()> {
    report["phases"]
        .as_array_mut()
        .ok_or("report phases is not an array")?
        .push(json!({"name":name,"status":status,"detail":detail}));
    Ok(())
}

fn append_capture(report: &mut Value, path: &Path) -> Result<()> {
    let digest = storage::sha256(File::open(path)?)?;
    report["captures"]
        .as_array_mut()
        .ok_or("report captures is not an array")?
        .push(json!({"path":path.display().to_string(),"sha256":digest}));
    Ok(())
}

fn run_slice(h: &mut Harness, options: &Options, report: &mut Value) -> Result<()> {
    let pid = h.start("slice")?;
    let (executed_file, executed_sha256) = verify_executed(options, pid)?;
    report["executed_pid"] = json!(pid);
    report["executed_file"] = json!(executed_file);
    report["executed_sha256"] = json!(executed_sha256);
    let app_window = h
        .window
        .parse::<u32>()
        .map_err(|error| format!("Harness did not publish its application window: {error}"))?;
    if find_window(h, pid, APP_TITLE)? != Some(app_window) {
        return Err("Harness window is not the Among Terminal window".into());
    }
    let script = h.base.join("start.script");
    let preferences_window = open_preferences(h, pid, &script, "open")?;
    wait_terminal_rects(h)?;

    choose_combo(h, preferences_window, TEXT_CONTROL, 2)?;
    if let Err(error) = wait_selection(h, "black-on-white", "gnome-terminal-default") {
        let png = h.base.join("slice-text-failure.png");
        let _ = capture_window(h, preferences_window, &png);
        let _ = append_capture(report, &png);
        report["slice_text_failure"] = json!({
            "error": error.to_string(),
            "prefs_terminal": latest_field(&h.text()?, "prefs_terminal").unwrap_or(""),
            "prefs_rects": latest_field(&h.text()?, "prefs_rects").unwrap_or(""),
        });
        return Err(error);
    }
    choose_combo(h, preferences_window, PALETTE_CONTROL, 2)?;
    if let Err(error) = wait_selection(h, "black-on-white", "tango") {
        let png = h.base.join("slice-palette-failure.png");
        let _ = capture_window(h, preferences_window, &png);
        let _ = append_capture(report, &png);
        report["slice_palette_failure"] = json!({
            "error": error.to_string(),
            "prefs_terminal": latest_field(&h.text()?, "prefs_terminal").unwrap_or(""),
            "prefs_rects": latest_field(&h.text()?, "prefs_rects").unwrap_or(""),
        });
        return Err(error);
    }
    close_preferences(h, pid, preferences_window, app_window)?;
    h.wait(
        10,
        |this| Ok(find_window(this, pid, PREFS_TITLE)?.is_none()),
    )?;
    h.xdo(&["windowactivate", "--sync", &app_window.to_string()])?;
    h.xdo(&["windowfocus", "--sync", &app_window.to_string()])?;
    std::thread::sleep(std::time::Duration::from_millis(300));

    let (calibration, cells) = capture_calibrated(h, app_window, "slice", report)?;
    let oracle = Oracle::read(&options.oracle)?;
    let pair = oracle.scheme("black-on-white")?;
    let palette = oracle.palette("tango")?;
    let (samples, failures) = sample_cells(
        &calibration,
        &cells,
        |cell| cell.kind != "block" && cell.kind != "readable",
        pair,
        palette,
    )?;
    report["samples"] = json!(samples);
    report["format"] =
        json!({"pixels":"R8G8B8A8 straight","alpha":"255 opaque","patch":"3x3 centre"});
    report["terminal_rect"] = json!(calibration.terminal);
    report["cell_pitch"] = json!({"x":calibration.pitch_x,"y":calibration.pitch_y});
    report["png"] = json!(calibration.png.display().to_string());
    report["raw"] = json!(calibration.raw.display().to_string());
    report["emitted_bytes"] = json!({
        "path": h.base.join("fixture-bytes.bin").display().to_string(),
        "sha256": storage::sha256(File::open(h.base.join("fixture-bytes.bin"))?)?,
    });
    report["fixture_map"] = json!({
        "path": h.base.join("fixture-map.txt").display().to_string(),
        "sha256": storage::sha256(File::open(h.base.join("fixture-map.txt"))?)?,
    });
    report["fixture_log"] = json!({
        "path": h.base.join("fixture.log").display().to_string(),
        "sha256": storage::sha256(File::open(h.base.join("fixture.log"))?)?,
    });
    if !failures.is_empty() {
        return Err(format!("colour samples differ: {}", failures.join("; ")).into());
    }
    append_phase(
        report,
        "vertical-slice",
        "passed",
        "Black on white + Tango screenshot and pixels",
    )?;
    Ok(())
}

fn run_owner_baseline(options: &Options, report: &mut Value) -> Result<()> {
    let mut h = fixture_harness(
        options,
        "among-owner-baseline",
        "await:owner\nfocus:terminal-text-background\n",
    )?;
    let result = (|| -> Result<()> {
        let pid = h.start("owner")?;
        let (executed_file, executed_sha256) = verify_executed(options, pid)?;
        report["owner_executed_file"] = json!(executed_file);
        report["owner_executed_sha256"] = json!(executed_sha256);
        let app_window = h
            .window
            .parse::<u32>()
            .map_err(|error| format!("Harness did not publish its application window: {error}"))?;
        if find_window(&h, pid, APP_TITLE)? != Some(app_window) {
            return Err("Harness window is not the Among Terminal window".into());
        }
        wait_terminal_rects(&mut h)?;
        let _ = capture_calibrated(&mut h, app_window, "owner-before", report)?;
        let script = h.base.join("start.script");
        let preferences_window = open_preferences(&mut h, pid, &script, "owner")?;
        choose_combo(&h, preferences_window, TEXT_CONTROL, 3)?;
        wait_selection(&mut h, "gray-on-black", "gnome-terminal-default")?;
        choose_combo(&h, preferences_window, PALETTE_CONTROL, 2)?;
        wait_selection(&mut h, "gray-on-black", "tango")?;
        close_preferences(&mut h, pid, preferences_window, app_window)?;
        h.wait(
            10,
            |this| Ok(find_window(this, pid, PREFS_TITLE)?.is_none()),
        )?;
        h.xdo(&["windowactivate", "--sync", &app_window.to_string()])?;
        h.xdo(&["windowfocus", "--sync", &app_window.to_string()])?;
        std::thread::sleep(std::time::Duration::from_millis(300));

        let (calibration, cells) =
            capture_calibrated(&mut h, app_window, "owner-gray-tango", report)?;
        let oracle = Oracle::read(&options.oracle)?;
        let pair = oracle.scheme("gray-on-black")?;
        let palette = oracle.palette("tango")?;
        let keep = |cell: &CellDef| {
            matches!(
                cell.label.as_str(),
                "default-bg" | "default-fg" | "ansi-fg-4" | "ansi-fg-12"
            )
        };
        let (samples, failures) = sample_cells(&calibration, &cells, keep, pair, palette)?;
        let background = sample_rgb(&samples, "default-bg")?;
        let blue = sample_rgb(&samples, "ansi-fg-4")?;
        let bright_blue = sample_rgb(&samples, "ansi-fg-12")?;
        let summary = json!({
            "before_capture": "owner-before",
            "capture": "owner-gray-tango",
            "text_background": "gray-on-black",
            "ansi_palette": "tango",
            "measured_background": pixels::hex(background),
            "ansi_4": pixels::hex(blue),
            "ansi_12": pixels::hex(bright_blue),
            "observed_contrast_4": contrast_ratio(blue, background),
            "observed_contrast_12": contrast_ratio(bright_blue, background),
            "note": "owner complaint evidence; no universal contrast threshold",
        });
        append_samples(report, "owner-gray-tango", samples)?;
        report["owner_baseline"] = summary;
        if !failures.is_empty() {
            return Err(format!("owner baseline samples differ: {}", failures.join("; ")).into());
        }
        append_phase(
            report,
            "owner-baseline",
            "passed",
            "Gray on black + Tango with measured ANSI 4/12 and observed contrast",
        )?;
        Ok(())
    })();
    let _ = h.stop(false);
    result
}

fn catalogue_script(oracle: &Oracle) -> String {
    let mut script = String::new();
    for (index, _) in oracle.scheme_order.iter().enumerate() {
        script.push_str(&format!(
            "await:text-{index}\nfocus:terminal-text-background\n"
        ));
    }
    for (index, _) in oracle.palette_order.iter().enumerate() {
        script.push_str(&format!(
            "await:palette-{index}\nfocus:terminal-ansi-palette\n"
        ));
    }
    script
}

fn run_catalogue(options: &Options, report: &mut Value) -> Result<()> {
    let oracle = Oracle::read(&options.oracle)?;
    let mut h = fixture_harness(options, "among-catalogue", &catalogue_script(&oracle))?;
    let result = (|| -> Result<()> {
        let pid = h.start("catalogue")?;
        let (executed_file, executed_sha256) = verify_executed(options, pid)?;
        report["catalogue_executed_file"] = json!(executed_file);
        report["catalogue_executed_sha256"] = json!(executed_sha256);
        let app_window = h
            .window
            .parse::<u32>()
            .map_err(|error| format!("Harness did not publish its application window: {error}"))?;
        if find_window(&h, pid, APP_TITLE)? != Some(app_window) {
            return Err("Harness window is not the Among Terminal window".into());
        }
        wait_terminal_rects(&mut h)?;
        let script = h.base.join("start.script");
        let mut current_text = "gnome-terminal-default";
        let mut current_palette = "gnome-terminal-default";
        let mut text_count = 0;
        let mut palette_count = 0;
        for (index, id) in oracle.scheme_order.iter().enumerate() {
            let preferences_window =
                open_preferences(&mut h, pid, &script, &format!("text-{index}"))?;
            choose_combo(&h, preferences_window, TEXT_CONTROL, index as u32 + 1)?;
            wait_selection(&mut h, id, current_palette)?;
            current_text = id.as_str();
            close_preferences(&mut h, pid, preferences_window, app_window)?;
            h.wait(
                10,
                |this| Ok(find_window(this, pid, PREFS_TITLE)?.is_none()),
            )?;
            h.xdo(&["windowactivate", "--sync", &app_window.to_string()])?;
            h.xdo(&["windowfocus", "--sync", &app_window.to_string()])?;
            let (calibration, cells) = capture_calibrated(
                &mut h,
                app_window,
                &format!("catalogue-text-{index}"),
                report,
            )?;
            let pair = oracle.scheme(id)?;
            // These default cells are measured from the explicit text/background
            // pair. The inherited default palette is intentionally absent from
            // the six-entry built-in oracle, and an empty slice is honest here:
            // no palette entry is consulted by this text-only sample.
            let keep = |cell: &CellDef| matches!(cell.kind.as_str(), "default-bg" | "default-fg");
            let (samples, failures) = sample_cells(&calibration, &cells, keep, pair, &[])?;
            append_samples(report, &format!("catalogue-text-{index}"), samples)?;
            if !failures.is_empty() {
                return Err(format!("scheme {id} samples differ: {}", failures.join("; ")).into());
            }
            text_count += 1;
        }
        for (index, id) in oracle.palette_order.iter().enumerate() {
            let preferences_window =
                open_preferences(&mut h, pid, &script, &format!("palette-{index}"))?;
            choose_combo(&h, preferences_window, PALETTE_CONTROL, index as u32 + 1)?;
            wait_selection(&mut h, current_text, id)?;
            current_palette = id.as_str();
            close_preferences(&mut h, pid, preferences_window, app_window)?;
            h.wait(
                10,
                |this| Ok(find_window(this, pid, PREFS_TITLE)?.is_none()),
            )?;
            h.xdo(&["windowactivate", "--sync", &app_window.to_string()])?;
            h.xdo(&["windowfocus", "--sync", &app_window.to_string()])?;
            let (calibration, cells) = capture_calibrated(
                &mut h,
                app_window,
                &format!("catalogue-palette-{index}"),
                report,
            )?;
            let pair = oracle.scheme(current_text)?;
            let palette = oracle.palette(id)?;
            let keep = |cell: &CellDef| matches!(cell.kind.as_str(), "palette-bg" | "palette-fg");
            let (samples, failures) = sample_cells(&calibration, &cells, keep, pair, palette)?;
            append_samples(report, &format!("catalogue-palette-{index}"), samples)?;
            if !failures.is_empty() {
                return Err(format!("palette {id} samples differ: {}", failures.join("; ")).into());
            }
            palette_count += 1;
        }
        report["catalogue"] = json!({
            "text_background_count": text_count,
            "palette_count": palette_count,
            "current_text": current_text,
            "current_palette": current_palette,
        });
        append_phase(
            report,
            "catalogue",
            "passed",
            "all 11 text/background schemes and 6 palettes through one binary run",
        )?;
        Ok(())
    })();
    let _ = h.stop(false);
    result
}

fn run_continuity(options: &Options, report: &mut Value) -> Result<()> {
    let mut h = fixture_harness(
        options,
        "among-continuity",
        "await:cont\nfocus:terminal-text-background\n",
    )?;
    let result = (|| -> Result<()> {
        let pid = h.start("continuity")?;
        verify_executed(options, pid)?;
        let app_window = h
            .window
            .parse::<u32>()
            .map_err(|error| format!("Harness did not publish its application window: {error}"))?;
        wait_terminal_rects(&mut h)?;
        let initial_pids = fixture_pids(&h)?;
        let first_pid = *initial_pids.first().ok_or("fixture PID missing")?;
        let first_start = proc_start_time(first_pid)?;
        let marker = h.base.join("continuity-marker");
        h.command(&format!("printf marker > {}", shell_quote(&marker)))?;
        h.wait(10, |_| Ok(marker.exists()))?;
        h.command("seq 1 200")?;
        // Wheel clicks go to the widget under the pointer, so put it inside
        // the terminal first; over the sidebar they would never scroll it.
        let pane = rect_of(&h.text()?, "terminal_rects", "1")?;
        h.xdo(&[
            "mousemove",
            "--window",
            &app_window.to_string(),
            &((pane[0] + pane[2] / 2.0) as i64).to_string(),
            &((pane[1] + pane[3] / 2.0) as i64).to_string(),
        ])?;
        h.xdo(&["click", "--repeat", "3", "--delay", "100", "4"])?;
        h.wait(10, |this| Ok(pane_scroll_moved(&this.text()?)))?;
        let terminal = rect_of(&h.text()?, "terminal_rects", "1")?;
        let x = (terminal[0] + 8.0) as i64;
        let y = (terminal[1] + 8.0) as i64;
        h.xdo(&[
            "mousemove",
            "--window",
            &app_window.to_string(),
            &x.to_string(),
            &y.to_string(),
        ])?;
        h.xdo(&["mousedown", "1"])?;
        h.xdo(&[
            "mousemove",
            "--window",
            &app_window.to_string(),
            &(x + 120).to_string(),
            &(y + 20).to_string(),
        ])?;
        h.xdo(&["mouseup", "1"])?;
        std::thread::sleep(std::time::Duration::from_millis(200));
        h.wait(10, |this| {
            Ok(latest_selection(&this.text()?).as_deref() == Some("[1]"))
        })?;
        let selection_before = latest_selection(&h.text()?).ok_or("initial selection missing")?;
        let scroll_before =
            pane_scroll_snapshot(&h.text()?).ok_or("initial pane_scroll missing")?;
        let script = h.base.join("start.script");
        let preferences_window = open_preferences(&mut h, pid, &script, "cont")?;
        choose_combo(&h, preferences_window, PALETTE_CONTROL, 2)?;
        wait_selection(&mut h, "gnome-terminal-default", "tango")?;
        let live_offset = h.text()?.len();
        close_preferences(&mut h, pid, preferences_window, app_window)?;
        h.wait(
            10,
            |this| Ok(find_window(this, pid, PREFS_TITLE)?.is_none()),
        )?;
        h.xdo(&["windowactivate", "--sync", &app_window.to_string()])?;
        h.xdo(&["windowfocus", "--sync", &app_window.to_string()])?;
        if !proc_alive(first_pid, first_start) {
            return Err("live colour change replaced the first fixture process".into());
        }
        h.wait(10, |this| {
            let text = this.text()?;
            Ok(
                latest_field_after(&text, live_offset, "terminal_selections")
                    .map(str::to_string)
                    .as_deref()
                    == Some(selection_before.as_str()),
            )
        })?;
        h.wait(10, |this| {
            let text = this.text()?;
            Ok(pane_scroll_snapshot_after(&text, live_offset) == Some(scroll_before))
        })?;
        let after_text = h.text()?;
        let selection_after = latest_field_after(&after_text, live_offset, "terminal_selections")
            .map(str::to_string)
            .ok_or("latest post-change selection missing")?;
        let scroll_after = pane_scroll_snapshot_after(&after_text, live_offset)
            .ok_or("latest post-change pane_scroll missing")?;
        if selection_after != selection_before || scroll_after != scroll_before {
            return Err("selection or scrollback changed across the live preset update".into());
        }
        let after_marker = h.base.join("continuity-after-marker");
        h.command(&format!("printf after > {}", shell_quote(&after_marker)))?;
        h.wait(10, |_| Ok(after_marker.exists()))?;
        if !proc_alive(first_pid, first_start) {
            return Err(
                "fresh command after live change did not use the first fixture process".into(),
            );
        }
        h.key("ctrl+shift+t")?;
        h.wait(15, |this| Ok(latest_workspaces(&this.text()?) == Some(2)))?;
        h.wait(15, |this| {
            Ok(fixture_pids(this)
                .map(|pids| pids.len() > initial_pids.len())
                .unwrap_or(false))
        })?;
        let workspace_pids = fixture_pids(&h)?;
        h.key("ctrl+Page_Up")?;
        h.wait(15, |this| {
            Ok(latest_active(&this.text()?) == Some(0)
                && latest_workspaces(&this.text()?) == Some(2))
        })?;
        if !proc_alive(first_pid, first_start) {
            return Err("hidden workspace lost the first fixture process".into());
        }
        let hidden_active = latest_active(&h.text()?);
        let hidden_workspaces = latest_workspaces(&h.text()?);
        h.key("ctrl+shift+n")?;
        h.wait(15, |this| Ok(latest_panes(&this.text()?) == Some(2)))?;
        h.wait(15, |this| {
            Ok(fixture_pids(this)
                .map(|pids| pids.len() > workspace_pids.len())
                .unwrap_or(false))
        })?;
        let split_pids = fixture_pids(&h)?;
        let split_new = split_pids
            .iter()
            .copied()
            .find(|candidate| !workspace_pids.contains(candidate))
            .ok_or("split did not start a second fixture process")?;
        let split_start = proc_start_time(split_new)?;
        if !proc_alive(first_pid, first_start) || !proc_alive(split_new, split_start) {
            return Err("split did not keep the first process and start a live second one".into());
        }
        report["continuity"] = json!({
            "first_pid": first_pid,
            "first_starttime": first_start,
            "after_live_first_alive": true,
            "selection_before": selection_before,
            "after_live_selection": selection_after,
            "scroll_before": scroll_before,
            "after_live_scroll": scroll_after,
            "after_live_command": "continuity-after-marker written by the same live process",
            "hidden_active": hidden_active,
            "hidden_workspaces": hidden_workspaces,
            "workspace_pids": workspace_pids,
            "split_pids": split_pids,
            "hidden_first_alive": true,
            "split_new_pid": split_new,
            "split_new_starttime": split_start,
            "split_first_alive": true,
        });
        append_phase(
            report,
            "continuity",
            "passed",
            "real typing, selection, scrollback, live PID, hidden workspace and split",
        )?;
        Ok(())
    })();
    let _ = h.stop(false);
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoldRelation {
    Normal,
    Bright,
}

fn classify_bold(normal: [u8; 4], bold: [u8; 4], bright: [u8; 4]) -> Option<BoldRelation> {
    if bold == normal {
        Some(BoldRelation::Normal)
    } else if bold == bright {
        Some(BoldRelation::Bright)
    } else {
        None
    }
}

fn run_osc_bold(options: &Options, report: &mut Value) -> Result<()> {
    let oracle = Oracle::read(&options.oracle)?;
    let mut h = fixture_harness(
        options,
        "among-osc-bold",
        // Each Preferences window needs its own await/focus pair: the focus
        // step is what reveals the Terminal page, and a script step runs once.
        "await:osc\nfocus:terminal-text-background\nawait:osc-again\nfocus:terminal-text-background\n",
    )?;
    let result = (|| -> Result<()> {
        let pid = h.start("osc-bold")?;
        verify_executed(options, pid)?;
        let app_window = h
            .window
            .parse::<u32>()
            .map_err(|error| format!("Harness did not publish its application window: {error}"))?;
        wait_terminal_rects(&mut h)?;
        let script = h.base.join("start.script");
        let preferences_window = open_preferences(&mut h, pid, &script, "osc")?;
        choose_combo(&h, preferences_window, PALETTE_CONTROL, 2)?;
        wait_selection(&mut h, "gnome-terminal-default", "tango")?;
        close_preferences(&mut h, pid, preferences_window, app_window)?;
        h.wait(
            10,
            |this| Ok(find_window(this, pid, PREFS_TITLE)?.is_none()),
        )?;
        h.xdo(&["windowactivate", "--sync", &app_window.to_string()])?;
        h.xdo(&["windowfocus", "--sync", &app_window.to_string()])?;
        let (bold_before_calibration, bold_before_cells) =
            capture_calibrated(&mut h, app_window, "bold-before", report)?;
        let before_normal = sample_one(&bold_before_calibration, &bold_before_cells, "normal-34")?;
        let before_bold = sample_one(&bold_before_calibration, &bold_before_cells, "bold-34")?;
        let before_bright = sample_one(&bold_before_calibration, &bold_before_cells, "bright-94")?;
        let before_bold_bright = sample_one(
            &bold_before_calibration,
            &bold_before_cells,
            "bold-bright-194",
        )?;
        h.command("printf '\\033]4;3;rgb:12/34/56\\033\\\\'")?;
        h.command("printf '\\033]10;rgb:aa/bb/cc\\033\\\\'")?;
        h.command("printf '\\033]11;rgb:10/20/30\\033\\\\'")?;
        std::thread::sleep(std::time::Duration::from_millis(300));
        let (before_calibration, before_cells) =
            capture_calibrated(&mut h, app_window, "osc-before", report)?;
        let before_override = sample_one(&before_calibration, &before_cells, "ansi-fg-3")?;
        let before_default_fg = sample_one(&before_calibration, &before_cells, "default-fg")?;
        let before_background = sample_one(&before_calibration, &before_cells, "default-bg")?;
        let before_extended = [
            "extended-16",
            "extended-231",
            "extended-232",
            "extended-255",
        ]
        .iter()
        .map(|label| sample_one(&before_calibration, &before_cells, label))
        .collect::<Result<Vec<_>>>()?;
        let before_true_bg = sample_one(&before_calibration, &before_cells, "true-bg")?;
        let before_true_fg = sample_one(&before_calibration, &before_cells, "true-fg")?;
        for (sample, index) in before_extended.iter().zip([16u32, 231, 232, 255]) {
            let expected = expected_extended(index).ok_or("no expected extended colour")?;
            if !pixels::close_rgb(sample.rgba, expected, 1) {
                return Err(format!(
                    "extended index {index} measured {:?}, expected {}",
                    sample.rgba,
                    pixels::hex(expected)
                )
                .into());
            }
        }
        let before_palette = oracle.palette("tango")?;
        if before_override.rgba != [0x12, 0x34, 0x56, 255]
            || before_default_fg.rgba != [0xaa, 0xbb, 0xcc, 255]
            || before_background.rgba != [0x10, 0x20, 0x30, 255]
            || before_true_bg.rgba != [17, 34, 51, 255]
            || before_true_fg.rgba != [68, 85, 102, 255]
        {
            return Err("program-issued OSC/truecolour did not reach the rendered cells".into());
        }
        let preferences_window = open_preferences(&mut h, pid, &script, "osc-again")?;
        select_palette(
            &mut h,
            preferences_window,
            1,
            "gnome-terminal-default",
            "gnome",
            report,
        )?;
        close_preferences(&mut h, pid, preferences_window, app_window)?;
        h.wait(
            10,
            |this| Ok(find_window(this, pid, PREFS_TITLE)?.is_none()),
        )?;
        h.xdo(&["windowactivate", "--sync", &app_window.to_string()])?;
        h.xdo(&["windowfocus", "--sync", &app_window.to_string()])?;
        let (after_calibration, after_cells) =
            capture_calibrated(&mut h, app_window, "osc-after", report)?;
        let after_override = sample_one(&after_calibration, &after_cells, "ansi-fg-3")?;
        let after_default_fg = sample_one(&after_calibration, &after_cells, "default-fg")?;
        let after_background = sample_one(&after_calibration, &after_cells, "default-bg")?;
        let after_normal = sample_one(&after_calibration, &after_cells, "normal-34")?;
        let after_bold = sample_one(&after_calibration, &after_cells, "bold-34")?;
        let after_bright = sample_one(&after_calibration, &after_cells, "bright-94")?;
        let after_bold_bright = sample_one(&after_calibration, &after_cells, "bold-bright-194")?;
        let after_extended = [
            "extended-16",
            "extended-231",
            "extended-232",
            "extended-255",
        ]
        .iter()
        .map(|label| sample_one(&after_calibration, &after_cells, label))
        .collect::<Result<Vec<_>>>()?;
        let after_true_bg = sample_one(&after_calibration, &after_cells, "true-bg")?;
        let after_true_fg = sample_one(&after_calibration, &after_cells, "true-fg")?;
        let after_palette = oracle.palette("gnome")?;
        if after_override.rgba != before_override.rgba
            || after_default_fg.rgba != before_default_fg.rgba
            || after_background.rgba != before_background.rgba
        {
            return Err("active OSC override did not survive the preset change".into());
        }
        if after_extended
            .iter()
            .zip(&before_extended)
            .any(|(after, before)| after.rgba != before.rgba)
            || after_true_bg.rgba != before_true_bg.rgba
            || after_true_fg.rgba != before_true_fg.rgba
        {
            return Err("extended or truecolour rendering changed with the preset".into());
        }
        report["osc"] = json!({
            "override_index": 3,
            "before_override": before_override.rgba,
            "after_override": after_override.rgba,
            "before_default_fg": before_default_fg.rgba,
            "after_default_fg": after_default_fg.rgba,
            "before_background": before_background.rgba,
            "after_background": after_background.rgba,
            "should_be_unchanged": {
                "ansi_3": true,
                "default_fg": true,
                "default_bg": true,
                "extended": before_extended.iter().map(|s| s.rgba).collect::<Vec<_>>(),
                "extended_after": after_extended.iter().map(|s| s.rgba).collect::<Vec<_>>(),
                "true_bg": before_true_bg.rgba,
                "true_fg": before_true_fg.rgba,
                "true_bg_after": after_true_bg.rgba,
                "true_fg_after": after_true_fg.rgba,
            },
            "before_palette_4": before_palette[4],
            "after_palette_4": after_palette[4],
        });
        append_phase(
            report,
            "osc",
            "passed",
            "OSC 4/10/11 and extended/truecolour unchanged; palette cells changed",
        )?;
        persist(&h, report)?;
        let before_relation =
            classify_bold(before_normal.rgba, before_bold.rgba, before_bright.rgba);
        let after_relation = classify_bold(after_normal.rgba, after_bold.rgba, after_bright.rgba);
        if before_relation.is_none()
            || before_relation != after_relation
            || before_bold_bright.rgba != before_bright.rgba
            || after_bold_bright.rgba != after_bright.rgba
        {
            return Err("bold-to-bright relation changed or is inconsistent".into());
        }
        if before_normal.rgba[..3] != before_palette[4][..]
            || before_bright.rgba[..3] != before_palette[12][..]
            || after_normal.rgba[..3] != after_palette[4][..]
            || after_bright.rgba[..3] != after_palette[12][..]
        {
            return Err("normal/bright blue no longer follows the selected palette".into());
        }
        report["bold"] = json!({
            "relation_before": before_relation.map(|value| format!("{value:?}")),
            "relation_after": after_relation.map(|value| format!("{value:?}")),
            "before_normal": before_normal.rgba,
            "before_bold": before_bold.rgba,
            "before_bright": before_bright.rgba,
            "after_normal": after_normal.rgba,
            "after_bold": after_bold.rgba,
            "after_bright": after_bright.rgba,
            "before_bold_bright": before_bold_bright.rgba,
            "after_bold_bright": after_bold_bright.rgba,
        });
        append_phase(
            report,
            "bold",
            "passed",
            "bold relation classified and unchanged",
        )?;
        Ok(())
    })();
    let _ = h.stop(false);
    result
}

fn run_layout(options: &Options, report: &mut Value) -> Result<()> {
    for (theme, scheme, gtk_theme) in [
        ("light", "light", None),
        ("dark", "dark", None),
        ("high-contrast", "light", Some("HighContrast")),
    ] {
        let mut h = fixture_harness(
            options,
            &format!("among-layout-{theme}"),
            "await:layout\nfocus:terminal-text-background\n",
        )?;
        let result = (|| -> Result<()> {
            h.env.insert("AMONG_COLOR_SCHEME".into(), scheme.into());
            if let Some(gtk_theme) = gtk_theme {
                h.env.insert("GTK_THEME".into(), gtk_theme.into());
            }
            let pid = h.start(theme)?;
            verify_executed(options, pid)?;
            let app_window = h.window.parse::<u32>().map_err(|error| {
                format!("Harness did not publish its application window: {error}")
            })?;
            wait_terminal_rects(&mut h)?;
            let script = h.base.join("start.script");
            let preferences_window = open_preferences(&mut h, pid, &script, "layout")?;
            h.wait(20, |this| {
                Ok(window_size(this, preferences_window)
                    .map(|(width, _)| width >= 820)
                    .unwrap_or(false))
            })?;
            h.wait(20, |this| Ok(tab_snapshot_is_current(&this.text()?)))?;
            let (width, _) = window_size(&h, preferences_window)?;
            if width != 820 {
                return Err(format!(
                    "Preferences default width is {width}, expected production 820"
                )
                .into());
            }
            let text = h.text()?;
            let min = latest_scalar(&text, "prefs_min").ok_or("latest prefs_min missing")?;
            let allocated =
                latest_scalar(&text, "prefs_allocated").ok_or("latest prefs_allocated missing")?;
            if min < 0.0 || allocated < min {
                return Err(
                    format!("Preferences toolbar min {min} exceeds allocated {allocated}").into(),
                );
            }
            for control in [TEXT_CONTROL, PALETTE_CONTROL] {
                if rect_of(&text, "prefs_rects", control).is_err() {
                    return Err(format!("Preferences control {control} is not mapped").into());
                }
            }
            let tabs = prefs_tabs(&text)?;
            validate_tabs(&tabs)?;
            let screenshot = h.base.join(format!("layout-{theme}.png"));
            capture_window(&h, preferences_window, &screenshot)?;
            append_capture(report, &screenshot)?;
            report["layout_themes"]
                .as_array_mut()
                .ok_or("report layout_themes is not an array")?
                .push(json!({
                    "theme": theme,
                    "app_window": app_window,
                    "preferences_width": width,
                    "toolbar_min": min,
                    "toolbar_allocated": allocated,
                    "tabs": tabs.iter().map(|tab| json!({"label":tab.label,"width":tab.width,"text_width":tab.text_width,"label_alloc":tab.label_alloc,"parent_width":tab.parent_width,"ellipsized":tab.ellipsized,"mapped":tab.mapped})).collect::<Vec<_>>(),
                    "controls": [TEXT_CONTROL, PALETTE_CONTROL],
                    "screenshot": screenshot.display().to_string(),
                }));
            close_preferences(&mut h, pid, preferences_window, app_window)?;
            Ok(())
        })();
        let _ = h.stop(false);
        result?;
    }
    append_phase(
        report,
        "layout",
        "passed",
        "real 820 default, toolbar min/allocated and controls in three themes",
    )?;
    Ok(())
}

fn fixture_harness(options: &Options, prefix: &str, script_text: &str) -> Result<Harness> {
    let root = scratch::root()?;
    let mut h = sessions::fresh(&options.binary, &root, prefix)?;
    let fixture_bytes = h.base.join("fixture-bytes.bin");
    let fixture_map = h.base.join("fixture-map.txt");
    let fixture_log = h.base.join("fixture.log");
    h.env
        .insert("SHELL".into(), options.fixture.clone().into_os_string());
    for (name, path) in [
        ("AMONG_COLOUR_FIXTURE_BYTES", &fixture_bytes),
        ("AMONG_COLOUR_FIXTURE_MAP", &fixture_map),
        ("AMONG_COLOUR_FIXTURE_LOG", &fixture_log),
    ] {
        h.env
            .insert(OsString::from(name), path.clone().into_os_string());
    }
    h.env.insert("AMONG_SELFCHECK_TICKS".into(), "900".into());
    let script = h.base.join("start.script");
    fs::write(&script, script_text)?;
    h.env.insert("AMONG_SCRIPT".into(), script.into_os_string());
    Ok(h)
}

fn phase_present(report: &Value, name: &str) -> bool {
    report["phases"].as_array().is_some_and(|phases| {
        phases
            .iter()
            .any(|phase| phase["name"].as_str() == Some(name))
    })
}

fn finish_own_phases(report: &mut Value, failed: Option<&str>) -> Result<()> {
    for name in OWN_PHASES {
        if phase_present(report, name) {
            continue;
        }
        let status = if failed == Some(name) {
            "failed"
        } else {
            "not-run"
        };
        append_phase(report, name, status, "owned phase did not complete")?;
    }
    Ok(())
}

fn finish_report(report: &mut Value) -> Result<()> {
    let phases = report["phases"]
        .as_array()
        .ok_or("report phases is not an array")?;
    let mut names = BTreeMap::new();
    for phase in phases {
        let name = phase["name"].as_str().ok_or("phase lacks name")?;
        if names
            .insert(name.to_string(), phase["status"].as_str())
            .is_some()
        {
            return Err(format!("duplicate phase {name}").into());
        }
    }
    if names.len() != PHASES.len() {
        return Err(format!(
            "expected {} unique phases, report has {}",
            PHASES.len(),
            names.len()
        )
        .into());
    }
    for (name, _) in PHASES {
        if names.get(name).copied().flatten() != Some("passed") {
            return Err(format!("phase {name} is not passed").into());
        }
    }
    report["status"] = json!("passed");
    Ok(())
}

fn persist(h: &Harness, report: &Value) -> Result<()> {
    write_report(&h.base.join("result.json"), report)
}

fn write_report(path: &Path, report: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(report)?)?;
    Ok(())
}

/// Entry point used by the `among-tools` dispatcher.
pub fn run(args: &[String]) -> Result<()> {
    let options = options(args)?;
    preferences::owned_display()?;
    let mut report = json!({
        "schema": 1,
        "status":"running",
        "boundary": options.boundary.label(),
        "executable": options.binary.display().to_string(),
        "executable_sha256": storage::sha256(File::open(&options.binary)?)?,
        "source_sha": options.source_sha.clone(),
        "schemas": options.schemas.as_ref().map(|path| path.display().to_string()),
        "fixture": options.fixture.display().to_string(),
        "fixture_sha256": storage::sha256(File::open(&options.fixture)?)?,
        "oracle": options.oracle.display().to_string(),
        "oracle_sha256": storage::sha256(File::open(&options.oracle)?)?,
        "captures": [],
        "calibrations": [],
        "layout_themes": [],
        "phases": [],
        "samples": [],
        "planned_unverified": [
            {"kind":"bold34","phase":"bold","reason":"bold policy is checked separately"},
            {"kind":"extended","phase":"baseline","reason":"compiled palette is checked separately"}
        ],
        "errors": [],
    });

    if let Some(schemas) = &options.schemas {
        let metadata = fs::metadata(schemas)?;
        if !metadata.is_dir() {
            return Err("gsettings schema path is not a directory".into());
        }
    }
    let mut h = fixture_harness(
        &options,
        "among-terminal-colours",
        "await:open\nfocus:terminal-text-background\n",
    )?;

    if let Err(error) = probe_rgba_order(&h, &h.base) {
        report["status"] = json!("failed");
        report["errors"] = json!([error.to_string()]);
        write_report(&h.base.join("result.json"), &report)?;
        return Err(error);
    }
    report["rgba_probe"] = json!({"status":"straight RGBA 8-bit; opaque calibration follows"});
    let slice = run_slice(&mut h, &options, &mut report);
    let _ = h.stop(false);
    persist(&h, &report)?;
    let mut outcome: Result<()> = slice;
    let mut failed_early = None;
    if outcome.is_ok() {
        outcome = run_owner_baseline(&options, &mut report);
        persist(&h, &report)?;
        if outcome.is_err() {
            failed_early = Some("owner-baseline");
        }
    } else {
        failed_early = Some("vertical-slice");
    }
    if outcome.is_ok() {
        outcome = run_catalogue(&options, &mut report);
        persist(&h, &report)?;
        if outcome.is_err() {
            failed_early = Some("catalogue");
        }
    }
    if outcome.is_ok() {
        outcome = source::run(&options, &mut report);
        persist(&h, &report)?;
    }
    let mut failed_own = None;
    if outcome.is_ok() {
        outcome = run_continuity(&options, &mut report);
        persist(&h, &report)?;
        if outcome.is_err() {
            failed_own = Some("continuity");
        }
    }
    if outcome.is_ok() {
        outcome = run_osc_bold(&options, &mut report);
        persist(&h, &report)?;
        if outcome.is_err() {
            failed_own = Some(if phase_present(&report, "osc") {
                "bold"
            } else {
                "osc"
            });
        }
    }
    if outcome.is_ok() {
        outcome = run_layout(&options, &mut report);
        persist(&h, &report)?;
        if outcome.is_err() {
            failed_own = Some("layout");
        }
    }
    if outcome.is_ok() {
        if let Err(error) = finish_report(&mut report) {
            outcome = Err(error);
            failed_own = Some("layout");
        }
    }
    match outcome {
        Ok(()) => {
            report["evidence"] = json!(h.base.display().to_string());
            write_report(&h.base.join("result.json"), &report)?;
            println!("Terminal colour evidence: {}", h.base.display());
            println!("Fixture evidence: {}", h.base.display());
            Ok(())
        }
        Err(error) => {
            report["status"] = json!("failed");
            report["errors"] = json!([error.to_string()]);
            if let Some(name) = failed_early {
                append_phase(&mut report, name, "failed", &error.to_string())?;
            }
            finish_own_phases(&mut report, failed_own)?;
            // A failure before a phase ran must still account for it.
            for (name, _) in PHASES {
                if !phase_present(&report, name) {
                    append_phase(&mut report, name, "not-run", "an earlier phase failed")?;
                }
            }
            report["evidence"] = json!(h.base.display().to_string());
            write_report(&h.base.join("result.json"), &report)?;
            println!("Terminal colour evidence: {}", h.base.display());
            println!("Fixture evidence: {}", h.base.display());
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangle_entries_stop_at_the_latest_field_bracket() -> Result<()> {
        let text = "among[tick7]: workspaces=1 active=0 panes=1 tree_boxes=[1:0+0+1948x1116] terminal_rects=[2:319+45+2x2] has_focus=false terminal_sizes=[198x65] sidebar_rows=[0:0+0+320x40 1:0+40+320x40]\n";
        assert!(rect_of(text, "terminal_rects", "1").is_err());
        assert_eq!(
            rect_of(text, "terminal_rects", "2")?,
            [319.0, 45.0, 2.0, 2.0]
        );
        let empty = "among[tick8]: terminal_rects=[] has_focus=false sidebar_rows=[0:0+0+320x40 1:0+40+320x40]\n";
        assert!(rect_of(empty, "terminal_rects", "1").is_err());
        assert!(rect_of(empty, "terminal_rects", "2").is_err());
        Ok(())
    }

    #[test]
    fn preferences_are_gone_only_after_a_tick_without_their_report() {
        let open = "among[tick7]: prefs_rects=[a:1+2+3x4] prefs_min=1\n\
                    among[tick7]: prefs_tabs=[]\n";
        assert!(!preferences_report_is_gone(open));
        assert!(!preferences_report_is_gone(""));
        let closed = format!("{open}among[tick8]: prefs_tabs=[]\n");
        assert!(preferences_report_is_gone(&closed));
    }

    #[test]
    fn latest_rect_field_wins_and_a_latest_empty_field_is_absence() -> Result<()> {
        let early =
            "among[tick1]: prefs_rects=[] prefs_min=-1 prefs_allocated=-1 prefs_terminal=[]\n\
                     among[tick1]: terminal_rects=[] has_focus=false\n";
        let current = "among[tick4]: prefs_rects=[terminal-text-background:10+20+300x40 terminal-ansi-palette:10+70+300x40] prefs_min=700 prefs_allocated=750 prefs_terminal=[]\n\
                       among[tick4]: terminal_rects=[1:30+40+900x600] has_focus=true\n";
        let text = format!("{early}{current}");
        assert_eq!(
            rect_of(&text, "prefs_rects", TEXT_CONTROL)?,
            [10.0, 20.0, 300.0, 40.0]
        );
        assert_eq!(
            rect_of(&text, "terminal_rects", "1")?,
            [30.0, 40.0, 900.0, 600.0]
        );
        let empty = format!("{text}among[tick5]: prefs_rects=[] prefs_min=-1 prefs_allocated=-1 prefs_terminal=[]\namong[tick5]: terminal_rects=[] has_focus=false\n");
        assert!(rect_of(&empty, "prefs_rects", TEXT_CONTROL).is_err());
        assert!(rect_of(&empty, "terminal_rects", "1").is_err());
        Ok(())
    }

    #[test]
    fn latest_prefs_terminal_map_does_not_reuse_an_old_id() -> Result<()> {
        let old = "among[tick1]: prefs_terminal=[(\"terminal-text-background\", \"old\", \"\")]\n";
        let new = "among[tick2]: prefs_terminal=[(\"terminal-text-background\", \"black-on-white\", \"custom\"), (\"terminal-ansi-palette\", \"tango\", \"\")]\n";
        let text = format!("{old}{new}");
        let map = prefs_terminal_map(&text)?;
        assert_eq!(
            map.get(TEXT_CONTROL).map(|(id, _)| id.as_str()),
            Some("black-on-white")
        );
        let empty = format!("{text}among[tick3]: prefs_terminal=[]\n");
        assert!(prefs_terminal_map(&empty)?.is_empty());
        Ok(())
    }

    #[test]
    fn partial_terminal_json_means_inherited_only_for_the_source_id() -> Result<()> {
        let partial = json!({
            "version": 1,
            "text_background": "black-on-white"
        });
        assert!(saved_appearance_matches(
            &partial,
            "text_background",
            "black-on-white"
        )?);
        assert!(saved_appearance_matches(
            &partial,
            "ansi_palette",
            "gnome-terminal-default"
        )?);
        assert!(!saved_appearance_matches(
            &partial,
            "ansi_palette",
            "tango"
        )?);
        let null = json!({"version":1,"ansi_palette":null});
        assert!(saved_appearance_matches(&null, "ansi_palette", "gnome-terminal-default").is_err());
        let wrong = json!({"version":1,"ansi_palette":7});
        assert!(
            saved_appearance_matches(&wrong, "ansi_palette", "gnome-terminal-default").is_err()
        );
        let wrong_text = json!({"version":1,"text_background":["black-on-white"]});
        assert!(
            saved_appearance_matches(&wrong_text, "text_background", "black-on-white").is_err()
        );
        Ok(())
    }

    #[test]
    fn rgb_rejects_more_than_three_channels() {
        assert!(rgb(&[json!(1), json!(2)]).is_err());
        assert!(rgb(&[json!(1), json!(2), json!(3)]).is_ok());
    }

    #[test]
    fn expected_palette_uses_the_exact_index() {
        let palette = vec![[1, 2, 3]; 16];
        let cell = CellDef {
            label: "x".into(),
            row: 0,
            col: 0,
            kind: "palette-fg".into(),
            index: 15,
        };
        assert_eq!(
            expected_for(&cell, ([7, 7, 7], [9, 9, 9]), &palette),
            Some([1, 2, 3])
        );
    }
}
