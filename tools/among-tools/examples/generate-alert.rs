//! Original Apache-2.0 asset source. Integer synthesis keeps regeneration exact.
use std::{fs::OpenOptions, io::Write, path::Path};

pub fn generate() -> Vec<u8> {
    let rate = 48_000u32;
    let frames = rate / 2;
    let size = frames * 2;
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + size).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * 2).to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(size.to_le_bytes());
    let mut phase = 0u32;
    for frame in 0..frames {
        let frequency = if frame < frames / 2 { 660 } else { 880 };
        phase = (phase + frequency) % rate;
        let triangle = if phase < rate / 2 {
            phase as i32 * 4 - rate as i32
        } else {
            rate as i32 * 3 - phase as i32 * 4
        };
        let edge = frame.min(frames - 1 - frame).min(rate / 50);
        let sample = (i64::from(triangle) * 4500 * i64::from(edge)
            / i64::from(rate)
            / i64::from(rate / 50)) as i16;
        bytes.extend(sample.to_le_bytes());
    }
    bytes
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .ok_or("usage: generate-alert NEW_WAV")?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(Path::new(&output))?
        .write_all(&generate())?;
    Ok(())
}

#[test]
fn asset_is_exact_original_integer_synthesis() {
    assert_eq!(
        generate(),
        include_bytes!("../../../data/audio/among-alert.wav")
    );
}
