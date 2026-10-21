use among::audio::{Sound, WavError, MAX_BYTES};
use std::{fs, path::PathBuf};

fn wave(tag: u16, bits: u16, channels: u16, rate: u32, frames: u32) -> Vec<u8> {
    let block = channels * (bits / 8);
    let mut format = Vec::new();
    format.extend(tag.to_le_bytes());
    format.extend(channels.to_le_bytes());
    format.extend(rate.to_le_bytes());
    format.extend((rate * u32::from(block)).to_le_bytes());
    format.extend(block.to_le_bytes());
    format.extend(bits.to_le_bytes());
    chunks(&[
        (b"fmt ", format),
        (b"data", vec![0; frames as usize * block as usize]),
    ])
}

fn chunks(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut bytes = b"RIFF\0\0\0\0WAVE".to_vec();
    for (name, chunk) in chunks {
        bytes.extend(*name);
        bytes.extend((chunk.len() as u32).to_le_bytes());
        bytes.extend(chunk);
        if chunk.len() & 1 != 0 {
            bytes.push(0);
        }
    }
    let size = (bytes.len() - 8) as u32;
    bytes[4..8].copy_from_slice(&size.to_le_bytes());
    bytes
}

#[test]
fn admits_all_declared_integer_float_formats_and_exact_duration_boundary() {
    for channels in [1, 2] {
        for (tag, bits) in [(1, 8), (1, 16), (1, 24), (1, 32), (3, 32), (3, 64)] {
            let bytes = wave(tag, bits, channels, 96_000, 100);
            let sound = Sound::from_bytes(&bytes).expect("valid format");
            assert_eq!(sound.info().bits, bits);
            assert_eq!(sound.info().channels, channels);
            assert_eq!(sound.info().floating_point, tag == 3);
        }
    }
    assert!(Sound::from_bytes(&wave(1, 16, 1, 10, 100)).is_ok());
    assert_eq!(
        Sound::from_bytes(&wave(1, 16, 1, 10, 101)).unwrap_err(),
        WavError::TooLong
    );
}

#[test]
fn checks_rate_frame_alignment_byte_rate_format_and_nonempty_data() {
    for (at, value) in [(22, 3u16), (32, 3), (34, 12)] {
        let mut bytes = wave(1, 16, 2, 48_000, 100);
        bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
        assert!(Sound::from_bytes(&bytes).is_err());
    }
    for rate in [0, 96_001, u32::MAX / 8] {
        assert!(Sound::from_bytes(&wave(1, 16, 1, rate, 10)).is_err());
    }
    for tag in [0, 2, 6, 7, 0x55, 0xffff] {
        assert_eq!(
            Sound::from_bytes(&wave(tag, 16, 1, 48_000, 10)).unwrap_err(),
            WavError::Unsupported
        );
    }
    assert!(Sound::from_bytes(&wave(1, 16, 1, 48_000, 0)).is_err());
    let original = wave(1, 16, 2, 48_000, 10);
    let format = original[20..36].to_vec();
    assert!(Sound::from_bytes(&chunks(&[(b"fmt ", format), (b"data", vec![0; 3])])).is_err());
    let mut bad_rate = original;
    bad_rate[28..32].copy_from_slice(&1u32.to_le_bytes());
    assert_eq!(
        Sound::from_bytes(&bad_rate).unwrap_err(),
        WavError::Inconsistent
    );
}

#[test]
fn checks_riff_and_chunk_extents_duplicate_chunks_and_padding() {
    let good = wave(1, 16, 1, 48_000, 10);
    for length in 0..good.len() {
        assert!(
            Sound::from_bytes(&good[..length]).is_err(),
            "truncated at {length}"
        );
    }
    let mut bad = good.clone();
    bad[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(Sound::from_bytes(&bad).unwrap_err(), WavError::Truncated);
    let fmt = good[20..36].to_vec();
    assert!(Sound::from_bytes(&chunks(&[
        (b"fmt ", fmt.clone()),
        (b"fmt ", fmt.clone()),
        (b"data", vec![0; 2])
    ]))
    .is_err());
    assert!(Sound::from_bytes(&chunks(&[
        (b"fmt ", fmt.clone()),
        (b"data", vec![0; 2]),
        (b"data", vec![0; 2])
    ]))
    .is_err());
    let mut odd = chunks(&[(b"JUNK", vec![0]), (b"fmt ", fmt), (b"data", vec![0; 2])]);
    let sound = Sound::from_bytes(&odd).expect("padded metadata");
    assert!(!sound.bytes().windows(4).any(|chunk| chunk == b"JUNK"));
    odd.pop();
    assert!(Sound::from_bytes(&odd).is_err());
}

#[test]
fn metadata_work_size_and_float_samples_are_bounded() {
    assert_eq!(
        Sound::from_bytes(&vec![0; MAX_BYTES + 1]).unwrap_err(),
        WavError::TooLarge
    );
    let good = wave(3, 64, 2, 96_000, 100);
    let fmt = good[20..36].to_vec();
    let mut many = vec![(b"JUNK" as &[u8; 4], Vec::new()); 4096];
    many.push((b"fmt ", fmt.clone()));
    many.push((b"data", vec![0; 16]));
    assert_eq!(
        Sound::from_bytes(&chunks(&many)).unwrap_err(),
        WavError::TooManyChunks
    );
    let mut exact = chunks(&[
        (b"fmt ", fmt),
        (b"JUNK", vec![0; MAX_BYTES - 68]),
        (b"data", vec![0; 16]),
    ]);
    assert_eq!(exact.len(), MAX_BYTES);
    assert!(Sound::from_bytes(&exact).is_ok());
    exact.push(0);
    assert_eq!(Sound::from_bytes(&exact).unwrap_err(), WavError::TooLarge);
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut bytes = wave(3, 32, 1, 48_000, 1);
        bytes[44..48].copy_from_slice(&value.to_le_bytes());
        assert_eq!(Sound::from_bytes(&bytes).unwrap_err(), WavError::Nonfinite);
    }
    let mut bytes = wave(3, 64, 1, 48_000, 1);
    bytes[44..52].copy_from_slice(&f64::NAN.to_le_bytes());
    assert_eq!(Sound::from_bytes(&bytes).unwrap_err(), WavError::Nonfinite);
}

#[test]
fn extensible_pcm_and_float_guids_valid_bits_and_channel_masks() {
    for tag in [1u16, 3] {
        let good = wave(tag, 32, 2, 48_000, 100);
        let mut fmt = good[20..36].to_vec();
        fmt[..2].copy_from_slice(&0xfffeu16.to_le_bytes());
        fmt.extend(22u16.to_le_bytes());
        fmt.extend(32u16.to_le_bytes());
        fmt.extend(3u32.to_le_bytes());
        fmt.extend(tag.to_le_bytes());
        fmt.extend([0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113]);
        let valid = chunks(&[(b"fmt ", fmt.clone()), (b"data", good[44..].to_vec())]);
        assert!(Sound::from_bytes(&valid).is_ok());
        fmt[24] = 7;
        assert!(
            Sound::from_bytes(&chunks(&[(b"fmt ", fmt.clone()), (b"data", vec![0; 8])])).is_err()
        );
        fmt[24] = tag as u8;
        fmt[20..24].copy_from_slice(&1u32.to_le_bytes());
        assert!(Sound::from_bytes(&chunks(&[(b"fmt ", fmt), (b"data", vec![0; 8])])).is_err());
    }
}

#[test]
fn owned_snapshot_survives_replacement_and_nonregular_inputs_are_rejected() {
    let directory = glib::mkdtemp(std::env::temp_dir().join("among-wav-test-XXXXXX"))
        .expect("private directory");
    let path = directory.join("selected.wav");
    fs::write(&path, wave(1, 16, 1, 48_000, 100)).expect("fixture");
    let snapshot = Sound::read_local(&path).expect("snapshot");
    fs::remove_file(&path).expect("remove original");
    fs::write(&path, b"not a WAV").expect("replacement");
    assert_eq!(snapshot.info().frames, 100);
    assert!(Sound::from_bytes(snapshot.bytes()).is_ok());
    assert_eq!(
        Sound::read_local(&PathBuf::from("/dev/null")).unwrap_err(),
        WavError::NotRegular
    );
    let fifo = directory.join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).expect("path");
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert_eq!(Sound::read_local(&fifo).unwrap_err(), WavError::NotRegular);
    fs::remove_dir_all(directory).expect("owned cleanup");
}
