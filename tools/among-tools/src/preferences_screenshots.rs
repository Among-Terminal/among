//! Turns the raw Preferences captures of the documentation test into the
//! published images: every picture is cropped by the same window frame, carries
//! no metadata (no timestamps, host or user text) and stays below the size
//! limit. Cropping and optimizing live here so a re-run gives the same bytes
//! from the same capture instead of depending on ad-hoc ImageMagick calls.

use crate::Result;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

/// The captures the documentation test writes, in the order they are listed in
/// `docs/preferences.md`.
pub const NAMES: [&str; 10] = [
    "plugins",
    "plugin-info",
    "plugin-access-review",
    "sources",
    "notifications",
    "keyboard",
    "terminal",
    "about",
    "plugins-light",
    "terminal-light",
];

/// Width of the window border the capture includes around the content, in pixels.
pub const FRAME: u32 = 5;

/// Published images must stay below this many bytes.
pub const MAX_BYTES: u64 = 300_000;

/// Width and height from a PNG's IHDR chunk.
pub fn png_size(bytes: &[u8]) -> Result<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.len() < 24 || bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return Err("not a PNG image".into());
    }
    let read =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Ok((read(16), read(20)))
}

/// Rejects a prepared image that is too large or whose width differs from the
/// first image's, which is how a mixed-up window size would show.
pub fn check(name: &str, bytes: &[u8], width: Option<u32>) -> Result<u32> {
    if bytes.len() as u64 >= MAX_BYTES {
        return Err(format!("{name}: {} bytes, the limit is {MAX_BYTES}", bytes.len()).into());
    }
    let (found, _) = png_size(bytes)?;
    if let Some(width) = width {
        if found != width {
            return Err(format!("{name}: width {found} differs from {width}").into());
        }
    }
    Ok(found)
}

/// Reads `raw/<name>.png` for every expected name and writes the prepared image
/// to `output/<name>.png`. A missing capture, an existing output file, or an
/// image that fails `check` is an error; nothing is skipped.
pub fn prepare(raw: &Path, output: &Path) -> Result<()> {
    fs::create_dir_all(output)?;
    let mut width = None;
    for name in NAMES {
        let source = raw.join(format!("{name}.png"));
        let target = output.join(format!("{name}.png"));
        if !source.is_file() {
            return Err(format!("missing capture {}", source.display()).into());
        }
        if target.exists() {
            return Err(format!("{} already exists", target.display()).into());
        }
        let status = Command::new("/usr/bin/convert")
            .env_clear()
            .arg(&source)
            .args(["-shave", &format!("{FRAME}x{FRAME}"), "+repage", "-strip"])
            .args(["-define", "png:exclude-chunk=all"])
            .args(["-define", "png:compression-level=9"])
            .arg(&target)
            .stdin(Stdio::null())
            .status()?;
        if !status.success() {
            return Err(format!("convert failed for {name}").into());
        }
        width = Some(check(name, &fs::read(&target)?, width)?);
        println!("prepared {}", target.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(width: u32, height: u32, padding: usize) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13];
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.resize(24 + padding, 0);
        bytes
    }

    #[test]
    fn the_size_is_read_from_the_header_and_other_data_is_refused() {
        assert_eq!(png_size(&header(818, 640, 0)).ok(), Some((818, 640)));
        assert!(png_size(b"GIF89a....................").is_err());
        assert!(png_size(&header(1, 1, 0)[..20]).is_err());
    }

    #[test]
    fn an_oversized_image_or_a_different_width_is_rejected() {
        assert_eq!(check("a", &header(818, 600, 0), None).ok(), Some(818));
        assert_eq!(check("b", &header(818, 700, 0), Some(818)).ok(), Some(818));
        assert!(check("c", &header(800, 700, 0), Some(818)).is_err());
        let largest_allowed = MAX_BYTES as usize - 1 - 24;
        assert!(check("d", &header(818, 600, largest_allowed), None).is_ok());
        assert!(check("e", &header(818, 600, largest_allowed + 1), None).is_err());
    }

    #[test]
    fn every_name_is_listed_once() {
        let mut names = NAMES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), NAMES.len());
    }
}
