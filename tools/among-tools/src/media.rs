//! Reproducible documentation media: run one scene in an owned display and
//! prepare the files the site publishes.
//!
//! The scene itself lives in the application test binary
//! (`app::plugin_tests::installation::doc_media`), because only that process
//! can build the real workspaces, panes and Preferences window. This module
//! owns the display, the output contract and the image/video preparation.

use crate::{display, preferences_screenshots::png_size, scratch, storage, Result};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub const STILLS_TEST: &str = "app::plugin_tests::installation::doc_media::capture_stills";
pub const NOTIFICATION_TEST: &str =
    "app::plugin_tests::installation::doc_media::capture_notification";
pub const PANES_TEST: &str = "app::plugin_tests::installation::doc_media::capture_panes";

const WIDE: u32 = 1600;
const WIDE_HEIGHT: u32 = 1000;
const NARROW: u32 = 1040;
const FRAME: u32 = 5;
const MARGIN: u32 = 80;

const WIDE_STILLS: [&str; 5] = [
    "main-window",
    "expanded-pane",
    "workspaces",
    "plugin-off",
    "plugin-on",
];
const NARROW_STILLS: [&str; 6] = [
    "plugins",
    "plugin-info",
    "plugin-access-review",
    "notifications",
    "keyboard",
    "terminal",
];

pub fn all(app_test: &Path, output: &Path) -> Result<PathBuf> {
    let stills = capture_stills(app_test, output)?;
    let panes = capture_panes(app_test, output)?;
    let notification = capture_notification(app_test, output)?;
    debug_assert_eq!(stills, panes);
    debug_assert_eq!(stills, notification);
    Ok(stills)
}

pub fn capture_stills(app_test: &Path, output: &Path) -> Result<PathBuf> {
    let raw = prepare_output(output, "raw/stills")?;
    run_test(app_test, STILLS_TEST, display::Mode::Media, &raw)?;
    for name in WIDE_STILLS {
        wide(
            &raw.join(format!("{name}.png")),
            &output.join(format!("{name}.png")),
        )?;
    }
    prompt(
        &raw.join("session-restore.png"),
        &output.join("session-restore.png"),
    )?;
    for name in NARROW_STILLS {
        narrow(
            &raw.join(format!("{name}.png")),
            &output.join(format!("{name}.png")),
        )?;
    }
    manifest(output)?;
    Ok(output.to_path_buf())
}

pub fn capture_panes(app_test: &Path, output: &Path) -> Result<PathBuf> {
    let raw = prepare_output(output, "raw/panes")?;
    run_test(app_test, PANES_TEST, display::Mode::Media, &raw)?;
    publish_video(
        &raw.join("panes.mp4"),
        output,
        "panes.mp4",
        "panes-poster.png",
        "6.0",
    )?;
    manifest(output)?;
    Ok(output.to_path_buf())
}

pub fn capture_notification(app_test: &Path, output: &Path) -> Result<PathBuf> {
    let raw = prepare_output(output, "raw/notification")?;
    run_test(app_test, NOTIFICATION_TEST, display::Mode::MediaGnome, &raw)?;
    // The test records while the scene settles, so the bell lands late. Publish
    // only the ring, banner and click; the idle prefix would look like a still.
    let recorded = raw.join("notification-return.mp4");
    let video = raw.join("notification-return-short.mp4");
    trim_recording(&recorded, &video, "16.3", "4.1")?;
    publish_video(
        &video,
        output,
        "notification-return.mp4",
        "notification-return-poster.png",
        "0.3",
    )?;
    let banner = output.join("desktop-notification.png");
    let status = Command::new("/usr/bin/ffmpeg")
        .env_clear()
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-ss",
            "0.3",
            "-i",
        ])
        .arg(&video)
        .args(["-frames:v", "1", "-vf", "crop=640:130:480:10"])
        .arg(&banner)
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        return Err("ffmpeg banner crop failed".into());
    }
    clean_png(&banner)?;
    manifest(output)?;
    Ok(output.to_path_buf())
}

/// Re-encode a time range so the published clip starts on a frame, not on the
/// encoder's previous keyframe.
fn trim_recording(raw: &Path, output: &Path, start: &str, duration: &str) -> Result<()> {
    let status = Command::new("/usr/bin/ffmpeg")
        .env_clear()
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-ss",
            start,
            "-i",
        ])
        .arg(raw)
        .args([
            "-t",
            duration,
            "-an",
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            "18",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ])
        .arg(output)
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        return Err("ffmpeg trim failed".into());
    }
    Ok(())
}

fn publish_video(
    raw_video: &Path,
    output: &Path,
    name: &str,
    poster_name: &str,
    poster_time: &str,
) -> Result<()> {
    if !raw_video.is_file() {
        return Err(format!("missing recording {}", raw_video.display()).into());
    }
    storage::atomic_write(output, Path::new(name), &fs::read(raw_video)?, 0o644)?;
    let poster = output.join(poster_name);
    let status = Command::new("/usr/bin/ffmpeg")
        .env_clear()
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-ss",
            poster_time,
            "-i",
        ])
        .arg(raw_video)
        .args(["-frames:v", "1"])
        .arg(&poster)
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(format!("ffmpeg poster extraction failed for {name}").into());
    }
    clean_png(&poster)
}

fn prepare_output(output: &Path, raw_name: &str) -> Result<PathBuf> {
    let raw = output.join(raw_name);
    if raw.exists() {
        return Err(format!("{} already exists", raw.display()).into());
    }
    let parent = output.parent().ok_or("output has no parent")?;
    fs::create_dir_all(parent)?;
    fs::create_dir_all(&raw)?;
    Ok(raw)
}

fn run_test(app_test: &Path, test: &str, mode: display::Mode, raw: &Path) -> Result<()> {
    let app_test = app_test.canonicalize()?;
    if !app_test.is_file() {
        return Err("application test executable must be a regular file".into());
    }
    std::env::set_var("AMONG_MEDIA_OUTPUT", raw);
    let args = vec![
        app_test.display().to_string(),
        test.into(),
        "--exact".into(),
        "--ignored".into(),
        "--test-threads=1".into(),
        "--nocapture".into(),
    ];
    let status = display::run(mode, &args, &scratch::root()?, Duration::from_secs(300))?;
    if !status.success() {
        return Err(format!("media scene failed; raw files remain in {}", raw.display()).into());
    }
    Ok(())
}

fn run_convert(args: &[&str], output: &Path) -> Result<()> {
    let status = Command::new("/usr/bin/convert")
        .env_clear()
        .args(args)
        .arg(output)
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(format!("convert failed for {}", output.display()).into());
    }
    metadata(output)?;
    Ok(())
}

fn wide(source: &Path, output: &Path) -> Result<()> {
    let bytes = fs::read(source)?;
    let (width, height) = png_size(&bytes)?;
    if width != WIDE || height != WIDE_HEIGHT {
        return Err(format!(
            "{} is {width}x{height}, expected {WIDE}x{WIDE_HEIGHT}",
            source.display()
        )
        .into());
    }
    run_convert(
        &[
            source.to_str().ok_or("source path is not UTF-8")?,
            "-strip",
            "-define",
            "png:exclude-chunk=all",
            "-define",
            "png:compression-level=9",
        ],
        output,
    )
}

fn prompt(source: &Path, output: &Path) -> Result<()> {
    let bytes = fs::read(source)?;
    let (width, height) = png_size(&bytes)?;
    if width != WIDE || height != WIDE_HEIGHT {
        return Err(format!(
            "{} is {width}x{height}, expected {WIDE}x{WIDE_HEIGHT}",
            source.display()
        )
        .into());
    }
    run_convert(
        &[
            source.to_str().ok_or("source path is not UTF-8")?,
            "-crop",
            "800x480+400+260",
            "+repage",
            "-strip",
            "-define",
            "png:exclude-chunk=all",
            "-define",
            "png:compression-level=9",
        ],
        output,
    )
}

fn narrow(source: &Path, output: &Path) -> Result<()> {
    let bytes = fs::read(source)?;
    let (width, height) = png_size(&bytes)?;
    if width <= FRAME * 2 || height <= FRAME * 2 {
        return Err(format!("{} is too small", source.display()).into());
    }
    let content_width = width - FRAME * 2;
    let content_height = height - FRAME * 2;
    if content_width != NARROW - MARGIN * 2 {
        return Err(format!(
            "{} content is {content_width} wide, expected {}",
            source.display(),
            NARROW - MARGIN * 2
        )
        .into());
    }
    let canvas = content_height + MARGIN * 2;
    run_convert(
        &[
            source.to_str().ok_or("source path is not UTF-8")?,
            "-shave",
            &format!("{FRAME}x{FRAME}"),
            "+repage",
            "-bordercolor",
            "#3a414d",
            "-border",
            "1",
            "-background",
            "#20262f",
            "-gravity",
            "center",
            "-extent",
            &format!("{NARROW}x{canvas}"),
            "-strip",
            "-define",
            "png:exclude-chunk=all",
            "-define",
            "png:compression-level=9",
        ],
        output,
    )
}

fn metadata(path: &Path) -> Result<()> {
    let bytes = fs::read(path)?;
    let (width, height) = png_size(&bytes)?;
    if width == 0 || height == 0 {
        return Err(format!("{} has no image area", path.display()).into());
    }
    Ok(())
}

fn clean_png(path: &Path) -> Result<()> {
    let temp = path.with_extension("clean.png");
    run_convert(
        &[
            path.to_str().ok_or("source path is not UTF-8")?,
            "-strip",
            "-define",
            "png:exclude-chunk=all",
            "-define",
            "png:compression-level=9",
        ],
        &temp,
    )?;
    fs::rename(&temp, path)?;
    metadata(path)
}

fn hash(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(fs::read(path)?);
    Ok(format!("{:x}", hasher.finalize()))
}

fn manifest(output: &Path) -> Result<()> {
    let mut assets = BTreeMap::new();
    for entry in fs::read_dir(output)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.file_name().is_some_and(|name| name == "media.json") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("media name is not UTF-8")?
            .to_string();
        let bytes = fs::read(&path)?;
        let entry = if name.ends_with(".png") {
            let (width, height) = png_size(&bytes)?;
            json!({"kind": "png", "width": width, "height": height, "bytes": bytes.len(), "sha256": hash(&path)?})
        } else {
            json!({"kind": "mp4", "bytes": bytes.len(), "sha256": hash(&path)?})
        };
        assets.insert(name, entry);
    }
    let source = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    let document = json!({
        "version": 1,
        "canvas": {
            "wide": [WIDE, WIDE_HEIGHT],
            "narrow_width": NARROW,
            "narrow_margin": MARGIN,
            "frame": FRAME,
            "backdrop": "#20262f",
            "border": "#3a414d"
        },
        "source": source,
        "assets": assets
    });
    storage::atomic_write(
        output,
        Path::new("media.json"),
        &serde_json::to_vec_pretty(&document)?,
        0o644,
    )?;
    Ok(())
}

pub fn verify(output: &Path) -> Result<()> {
    let document = storage::parse_json(&fs::read(output.join("media.json"))?)?;
    let assets = document["assets"]
        .as_object()
        .ok_or("media.json has no assets")?;
    for (name, entry) in assets {
        let path = output.join(name);
        if !path.is_file() {
            return Err(format!("missing asset {name}").into());
        }
        let bytes = fs::read(&path)?;
        if entry["sha256"].as_str() != Some(hash(&path)?.as_str()) {
            return Err(format!("{name} does not match its hash").into());
        }
        if entry["bytes"].as_u64() != Some(bytes.len() as u64) {
            return Err(format!("{name} does not match its size").into());
        }
        if entry["kind"] == "png" {
            let (width, height) = png_size(&bytes)?;
            if entry["width"].as_u64() != Some(u64::from(width))
                || entry["height"].as_u64() != Some(u64::from(height))
            {
                return Err(format!("{name} does not match its recorded dimensions").into());
            }
        }
    }
    for name in ["notification-return.mp4", "panes.mp4"] {
        let video = output.join(name);
        if !video.is_file() {
            continue;
        }
        let probe = Command::new("/usr/bin/ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=width,height,r_frame_rate",
                "-of",
                "csv=p=0",
            ])
            .arg(&video)
            .output()?;
        if !probe.status.success() {
            return Err(format!("ffprobe failed for {name}").into());
        }
        let text = String::from_utf8_lossy(&probe.stdout);
        if !text.trim().eq_ignore_ascii_case("1600,1000,30/1")
            && !text.trim().eq_ignore_ascii_case("1600,1000,30")
        {
            return Err(format!("unexpected {name} stream: {}", text.trim()).into());
        }
    }
    println!("PASS {} media assets in {}", assets.len(), output.display());
    Ok(())
}
