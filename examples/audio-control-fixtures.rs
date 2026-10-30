//! Identical source-bound bytes for normal EOS and active-revocation controls.
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .ok_or("new fixture directory required")?;
    let root = Path::new(&directory);
    fs::create_dir(root)?;
    let frames = 8_u32 * 48_000;
    let mut bytes = b"RIFF".to_vec();
    bytes.extend((36 + frames * 2).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(48_000_u32.to_le_bytes());
    bytes.extend(96_000_u32.to_le_bytes());
    bytes.extend(2_u16.to_le_bytes());
    bytes.extend(16_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((frames * 2).to_le_bytes());
    for frame in 0..frames {
        let angle = f64::from(frame) * 880.0 * std::f64::consts::TAU / 48_000.0;
        bytes.extend(((angle.sin() * 8192.0) as i16).to_le_bytes());
    }
    for name in ["tone-eight.wav", "tone-recovery.wav"] {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(root.join(name))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    Ok(())
}
