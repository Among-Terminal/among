//! Bounded WAV admission before any native decoder receives owned bytes.

use std::{
    fmt,
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::OpenOptionsExt,
    },
    path::Path,
    sync::Arc,
};

pub const MAX_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_RATE: u32 = 96_000;
pub const MAX_SECONDS: u32 = 10;
const MAX_CHUNKS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WavError {
    Unavailable,
    NotRegular,
    TooLarge,
    Truncated,
    Unsupported,
    Inconsistent,
    TooLong,
    TooManyChunks,
    Nonfinite,
}

impl fmt::Display for WavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "The selected sound could not be read.",
            Self::NotRegular => "Select a local regular WAV file.",
            Self::TooLarge => "The sound exceeds 10 MiB.",
            Self::Truncated => "The WAV is truncated or has an invalid RIFF size.",
            Self::Unsupported => "Select an uncompressed PCM or IEEE-float WAV.",
            Self::Inconsistent => "The WAV format, rate or frame sizes are inconsistent.",
            Self::TooLong => "The sound exceeds 10 seconds.",
            Self::TooManyChunks => "The WAV contains too many metadata chunks.",
            Self::Nonfinite => "The WAV contains non-finite floating-point samples.",
        })
    }
}
impl std::error::Error for WavError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WavInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits: u16,
    pub floating_point: bool,
    pub frames: u32,
}

/// Only this constructor can admit bytes to playback. Metadata is stripped so
/// the decoder cannot reinterpret ancillary chunks after validation.
#[derive(Debug, Clone)]
pub struct Sound {
    bytes: Arc<[u8]>,
    info: WavInfo,
    sealed: Arc<File>,
}

fn u16le(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}
fn u32le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

impl Sound {
    /// Run file I/O away from the GUI thread (Audio::select_local does this).
    /// NONBLOCK avoids hanging on a FIFO before its descriptor can be rejected.
    pub fn read_local(path: &Path) -> Result<Self, WavError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| WavError::Unavailable)?;
        let metadata = file.metadata().map_err(|_| WavError::Unavailable)?;
        if !metadata.is_file() {
            return Err(WavError::NotRegular);
        }
        if metadata.len() > MAX_BYTES as u64 {
            return Err(WavError::TooLarge);
        }
        let mut bytes = Vec::new();
        file.take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| WavError::Unavailable)?;
        Self::from_bytes(&bytes)
    }

    pub fn builtin() -> Result<Self, WavError> {
        Self::from_bytes(include_bytes!("../../data/audio/among-alert.wav"))
    }

    pub fn info(&self) -> WavInfo {
        self.info
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn decoder_path(&self) -> String {
        format!("/proc/self/fd/{}", self.sealed.as_raw_fd())
    }

    pub(crate) fn decoder_file(&self) -> std::io::Result<File> {
        self.sealed.try_clone()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, WavError> {
        if bytes.len() > MAX_BYTES {
            return Err(WavError::TooLarge);
        }
        if bytes.len() < 12 {
            return Err(WavError::Truncated);
        }
        if &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return Err(WavError::Unsupported);
        }
        if u64::from(u32le(bytes, 4)) + 8 != bytes.len() as u64 {
            return Err(WavError::Truncated);
        }
        let (mut format, mut data) = (None, None);
        let mut offset = 12usize;
        let mut chunks = 0;
        while offset < bytes.len() {
            chunks += 1;
            if chunks > MAX_CHUNKS {
                return Err(WavError::TooManyChunks);
            }
            let header = bytes.get(offset..offset + 8).ok_or(WavError::Truncated)?;
            let size = u32le(header, 4) as usize;
            let start = offset + 8;
            let end = start.checked_add(size).ok_or(WavError::Truncated)?;
            let chunk = bytes.get(start..end).ok_or(WavError::Truncated)?;
            match &header[..4] {
                b"fmt " if format.is_none() => format = Some(chunk),
                b"data" if data.is_none() => data = Some(chunk),
                b"fmt " | b"data" => return Err(WavError::Inconsistent),
                _ => {}
            }
            offset = end.checked_add(size & 1).ok_or(WavError::Truncated)?;
            if offset > bytes.len() {
                return Err(WavError::Truncated);
            }
        }
        let format = format.ok_or(WavError::Inconsistent)?;
        let data = data.ok_or(WavError::Inconsistent)?;
        if format.len() < 16 {
            return Err(WavError::Truncated);
        }
        let mut tag = u16le(format, 0);
        let channels = u16le(format, 2);
        let rate = u32le(format, 4);
        let bits = u16le(format, 14);
        if tag == 0xfffe {
            if format.len() != 40 || u16le(format, 16) != 22 {
                return Err(WavError::Unsupported);
            }
            let valid = u16le(format, 18);
            let mask = u32le(format, 20);
            let guid = &format[24..40];
            if guid[2..] != [0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113] {
                return Err(WavError::Unsupported);
            }
            tag = u16le(guid, 0);
            if valid == 0
                || valid > bits
                || (tag == 3 && valid != bits)
                || (mask != 0 && mask.count_ones() != u32::from(channels))
            {
                return Err(WavError::Inconsistent);
            }
        } else if format.len() != 16 && !(format.len() == 18 && u16le(format, 16) == 0) {
            return Err(WavError::Unsupported);
        }
        let floating_point = match tag {
            1 if matches!(bits, 8 | 16 | 24 | 32) => false,
            3 if matches!(bits, 32 | 64) => true,
            _ => return Err(WavError::Unsupported),
        };
        if !matches!(channels, 1 | 2) || rate == 0 || rate > MAX_RATE {
            return Err(WavError::Inconsistent);
        }
        let block = u32::from(channels) * u32::from(bits / 8);
        if u32::from(u16le(format, 12)) != block
            || u32le(format, 8) != rate * block
            || data.is_empty()
            || data.len() % block as usize != 0
        {
            return Err(WavError::Inconsistent);
        }
        let frames = (data.len() / block as usize) as u32;
        if frames > rate * MAX_SECONDS {
            return Err(WavError::TooLong);
        }
        if floating_point {
            for sample in data.chunks_exact((bits / 8) as usize) {
                let finite = if bits == 32 {
                    f32::from_bits(u32le(sample, 0)).is_finite()
                } else {
                    let mut value = [0; 8];
                    value.copy_from_slice(sample);
                    f64::from_le_bytes(value).is_finite()
                };
                if !finite {
                    return Err(WavError::Nonfinite);
                }
            }
        }
        let mut canonical = Vec::with_capacity(20 + format.len() + 8 + data.len() + 1);
        canonical.extend(b"RIFF\0\0\0\0WAVEfmt ");
        canonical.extend((format.len() as u32).to_le_bytes());
        canonical.extend(format);
        canonical.extend(b"data");
        canonical.extend((data.len() as u32).to_le_bytes());
        canonical.extend(data);
        if data.len() & 1 != 0 {
            canonical.push(0);
        }
        let size = (canonical.len() - 8) as u32;
        canonical[4..8].copy_from_slice(&size.to_le_bytes());
        // GTK 4.14's GStreamer backend asserts on GInputStream. A sealed memfd
        // provides its supported file URI without reopening the user's path.
        let descriptor = unsafe {
            libc::memfd_create(
                c"among-sound".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if descriptor < 0 {
            return Err(WavError::Unavailable);
        }
        // memfd_create returned a new owned descriptor; File closes every exit.
        let mut sealed = unsafe { File::from_raw_fd(descriptor) };
        sealed
            .write_all(&canonical)
            .map_err(|_| WavError::Unavailable)?;
        let seals =
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
        if unsafe { libc::fcntl(sealed.as_raw_fd(), libc::F_ADD_SEALS, seals) } < 0 {
            return Err(WavError::Unavailable);
        }
        Ok(Self {
            bytes: canonical.into(),
            info: WavInfo {
                sample_rate: rate,
                channels,
                bits,
                floating_point,
                frames,
            },
            sealed: Arc::new(sealed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_descriptor_is_sealed_against_mutation_resize_and_seal_removal() {
        let sound = Sound::builtin().expect("original valid asset");
        let fd = sound.sealed.as_raw_fd();
        let seals = unsafe { libc::fcntl(fd, libc::F_GET_SEALS) };
        assert_eq!(
            seals,
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL
        );
        assert_eq!(unsafe { libc::pwrite(fd, b"x".as_ptr().cast(), 1, 0) }, -1);
        assert_eq!(unsafe { libc::ftruncate(fd, 0) }, -1);
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, 0) }, -1);
    }
}
