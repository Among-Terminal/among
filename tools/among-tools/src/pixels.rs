//! Bounded raw-RGBA reading and solid-patch sampling for colour acceptance.
//!
//! The X11 checker captures an opaque window with ImageMagick `import`, asks
//! `convert` for 8-bit straight `R,G,B,A` bytes, and hands those bytes here.
//! This module deliberately does not decode PNG itself: the existing display
//! prerequisite already provides ImageMagick, and keeping the byte reader
//! dependency-free avoids adding GTK/GDK to the standalone tool build.

use std::fs;
use std::path::Path;

use crate::Result;

/// The largest raw capture this checker will accept. The owned display is
/// 1920x1200, so a 4096x4096 RGBA image is already generous and keeps a bad
/// `identify` answer from allocating without bound.
const MAX_BYTES: u64 = 4096 * 4096 * 4;

/// An 8-bit, straight (not premultiplied) RGBA image.
#[derive(Debug, Clone)]
pub struct RgbaImage {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

/// One solid sample and the largest channel difference inside its patch.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub rgba: [u8; 4],
    pub max_delta: u8,
}

/// An inclusive-exclusive pixel rectangle.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl RgbaImage {
    /// Read exactly `width * height * 4` bytes from a raw `rgba:` file.
    pub fn read_raw(path: &Path, width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 {
            return Err("raw RGBA image has a zero dimension".into());
        }
        let expected = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or("raw RGBA image dimensions overflow")?;
        if expected > MAX_BYTES {
            return Err(format!(
                "raw RGBA image is {expected} bytes, above the {MAX_BYTES}-byte bound"
            )
            .into());
        }
        let metadata = fs::metadata(path)?;
        if !metadata.is_file() || metadata.len() != expected {
            return Err(format!(
                "raw RGBA file {} is {} bytes, expected {expected}",
                path.display(),
                metadata.len()
            )
            .into());
        }
        let data = fs::read(path)?;
        if data.len() as u64 != expected {
            return Err("raw RGBA file changed while it was read".into());
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }

    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = ((u64::from(y) * u64::from(self.width) + u64::from(x)) * 4) as usize;
        Some([
            self.data[offset],
            self.data[offset + 1],
            self.data[offset + 2],
            self.data[offset + 3],
        ])
    }

    /// Sample an odd-sized patch and require every pixel to agree within
    /// `tolerance`. The centre pixel is reported as the sample value.
    pub fn sample_solid(&self, x: u32, y: u32, size: u32, tolerance: u8) -> Result<Sample> {
        if size == 0 || size.is_multiple_of(2) {
            return Err("solid sampling patch must be odd and non-zero".into());
        }
        let radius = size / 2;
        let mut first: Option<[u8; 4]> = None;
        let mut max_delta = 0u8;
        for dy in 0..size {
            for dx in 0..size {
                let px = x
                    .checked_sub(radius)
                    .and_then(|value| value.checked_add(dx))
                    .ok_or("sample patch leaves the image on the left")?;
                let py = y
                    .checked_sub(radius)
                    .and_then(|value| value.checked_add(dy))
                    .ok_or("sample patch leaves the image on the top")?;
                let pixel = self.pixel(px, py).ok_or("sample patch leaves the image")?;
                if let Some(first) = first {
                    for channel in 0..4 {
                        max_delta = max_delta.max(first[channel].abs_diff(pixel[channel]));
                    }
                } else {
                    first = Some(pixel);
                }
            }
        }
        if max_delta > tolerance {
            return Err(format!(
                "sample patch at {x},{y} is not solid: maximum channel delta {max_delta} exceeds {tolerance}"
            )
            .into());
        }
        Ok(Sample {
            rgba: first.ok_or("sample patch is empty")?,
            max_delta,
        })
    }

    /// Find the largest four-connected component of one exact opaque colour
    /// that is also a filled rectangle. Isolated anti-aliased marker-coloured
    /// pixels cannot enlarge the calibration block's bounds.
    pub fn largest_solid_component(&self, bounds: Bounds, rgba: [u8; 4]) -> Option<Bounds> {
        let x_end = bounds.x.saturating_add(bounds.width).min(self.width);
        let y_end = bounds.y.saturating_add(bounds.height).min(self.height);
        let width = x_end.saturating_sub(bounds.x);
        let height = y_end.saturating_sub(bounds.y);
        if width == 0 || height == 0 {
            return None;
        }
        let cells = usize::try_from(u64::from(width) * u64::from(height)).ok()?;
        let mut visited = vec![false; cells];
        let index = |x: u32, y: u32| -> usize {
            ((y - bounds.y) as usize) * width as usize + (x - bounds.x) as usize
        };
        let mut best = None;
        let mut best_area = 0u64;
        for y in bounds.y..y_end {
            for x in bounds.x..x_end {
                if visited[index(x, y)] || self.pixel(x, y) != Some(rgba) {
                    continue;
                }
                let mut pending = vec![(x, y)];
                visited[index(x, y)] = true;
                let mut pixels = 0u64;
                let (mut left, mut top, mut right, mut bottom) = (x, y, x + 1, y + 1);
                while let Some((px, py)) = pending.pop() {
                    pixels += 1;
                    left = left.min(px);
                    top = top.min(py);
                    right = right.max(px + 1);
                    bottom = bottom.max(py + 1);
                    for (nx, ny) in [
                        (px.checked_sub(1), Some(py)),
                        (px.checked_add(1), Some(py)),
                        (Some(px), py.checked_sub(1)),
                        (Some(px), py.checked_add(1)),
                    ] {
                        let (Some(nx), Some(ny)) = (nx, ny) else {
                            continue;
                        };
                        if nx < bounds.x
                            || nx >= x_end
                            || ny < bounds.y
                            || ny >= y_end
                            || visited[index(nx, ny)]
                            || self.pixel(nx, ny) != Some(rgba)
                        {
                            continue;
                        }
                        visited[index(nx, ny)] = true;
                        pending.push((nx, ny));
                    }
                }
                let area = u64::from(right - left) * u64::from(bottom - top);
                if pixels == area && pixels > best_area {
                    best_area = pixels;
                    best = Some(Bounds {
                        x: left,
                        y: top,
                        width: right - left,
                        height: bottom - top,
                    });
                }
            }
        }
        best
    }
}

/// Compare the visible RGB channels with an oracle value after the live
/// calibration patch has established the capture path.
pub fn close_rgb(observed: [u8; 4], expected: [u8; 3], tolerance: u8) -> bool {
    observed[3] == 255
        && observed[0].abs_diff(expected[0]) <= tolerance
        && observed[1].abs_diff(expected[1]) <= tolerance
        && observed[2].abs_diff(expected[2]) <= tolerance
}

pub fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> RgbaImage {
        RgbaImage {
            width: 3,
            height: 3,
            data: [7, 8, 9, 255].repeat(9),
        }
    }

    #[test]
    fn a_solid_patch_is_sampled_from_its_centre() -> Result<()> {
        let sample = image().sample_solid(1, 1, 3, 0)?;
        assert_eq!(sample.rgba, [7, 8, 9, 255]);
        assert_eq!(sample.max_delta, 0);
        Ok(())
    }

    #[test]
    fn a_non_solid_patch_is_refused() {
        let mut image = image();
        image.data[0] = 10;
        assert!(image.sample_solid(1, 1, 3, 0).is_err());
    }

    #[test]
    fn exact_calibration_component_ignores_isolated_marker_pixels() -> Result<()> {
        let mut image = RgbaImage {
            width: 20,
            height: 20,
            data: [0, 0, 0, 255].repeat(400),
        };
        for y in 2..6 {
            for x in 3..7 {
                let offset = ((y * 20 + x) * 4) as usize;
                image.data[offset..offset + 4].copy_from_slice(&[1, 2, 3, 255]);
            }
        }
        let stray = (18 * 20 + 18) * 4;
        image.data[stray..stray + 4].copy_from_slice(&[1, 2, 3, 255]);
        let bounds = image
            .largest_solid_component(
                Bounds {
                    x: 0,
                    y: 0,
                    width: 20,
                    height: 20,
                },
                [1, 2, 3, 255],
            )
            .ok_or("calibration colour missing")?;
        assert_eq!(
            (bounds.x, bounds.y, bounds.width, bounds.height),
            (3, 2, 4, 4)
        );
        Ok(())
    }

    #[test]
    fn alpha_must_be_opaque_for_a_colour_match() {
        assert!(!close_rgb([1, 2, 3, 254], [1, 2, 3], 0));
        assert!(close_rgb([1, 2, 3, 255], [1, 2, 3], 0));
    }
}
