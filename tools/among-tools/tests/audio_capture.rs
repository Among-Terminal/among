use among_tools::{audio_capture, audio_package, storage, Result};
use std::fs;

#[test]
fn safety_identity_rejects_physical_extra_wrong_socket_or_incomplete_server() -> Result<()> {
    let info = "Server String: unix:/owned/pulse.sock\nDefault Sink: among_audit\n";
    let sinks = "0\tamong_audit\tmodule-null-sink.c\ts16le\n";
    let modules = "0\tmodule-native-protocol-unix\tsocket=/owned/pulse.sock\n1\tmodule-null-sink\tsink_name=among_audit\n";
    audio_capture::identity(info, sinks, modules, "unix:/owned/pulse.sock")?;
    assert!(audio_capture::identity(info, sinks, modules, "unix:/personal/pulse.sock").is_err());
    assert!(audio_capture::identity(
        info,
        &format!("{sinks}1\tphysical\tmodule-alsa-card.c\n"),
        modules,
        "unix:/owned/pulse.sock"
    )
    .is_err());
    assert!(audio_capture::identity(
        info,
        sinks,
        &format!("{modules}2\tmodule-alsa-card\n"),
        "unix:/owned/pulse.sock"
    )
    .is_err());
    assert!(audio_capture::identity(
        info,
        sinks,
        "0\tmodule-null-sink\n",
        "unix:/owned/pulse.sock"
    )
    .is_err());
    Ok(())
}

#[test]
fn analysis_requires_real_expected_waveform_and_nonempty_silence() -> Result<()> {
    let mut tone = Vec::new();
    for i in 0..48_000 {
        tone.extend(
            (((i as f64 * 880.0 * std::f64::consts::TAU / 48_000.0).sin() * 8000.0) as i16)
                .to_le_bytes(),
        );
    }
    audio_capture::analyze(&tone, true)?;
    assert!(audio_capture::analyze(&tone, false).is_err());
    assert!(audio_capture::analyze(&vec![0; 96_000], true).is_err());
    audio_capture::analyze(&vec![0; 96_000], false)?;
    assert!(audio_capture::analyze(&[], false).is_err());
    assert!(audio_capture::analyze(&vec![1; 96_000], true).is_err());
    Ok(())
}

#[test]
fn recovery_capture_cannot_borrow_the_initial_pre_stall_tone() -> Result<()> {
    let tone: Vec<_> = (0..48_000)
        .flat_map(|frame| {
            (((frame as f64 * 880.0 * std::f64::consts::TAU / 48_000.0).sin() * 8000.0) as i16)
                .to_le_bytes()
        })
        .collect();
    assert!(audio_capture::recovered_output(&tone).is_err());
    let mut two = tone.clone();
    two.extend(vec![0; 48_000]);
    two.extend(&tone);
    let observed = audio_capture::recovered_output(&two)?;
    assert!(
        observed["recovery"]["start_frame"]
            .as_u64()
            .ok_or("recovery frame absent")?
            > observed["initial"]["end_frame"]
                .as_u64()
                .ok_or("initial frame absent")?
    );
    let mut only_initial = tone;
    only_initial.extend(vec![0; 96_000]);
    assert!(audio_capture::recovered_output(&only_initial).is_err());
    Ok(())
}

#[test]
fn probe_receipt_binds_architecture_artifact_source_and_real_binary_bytes() -> Result<()> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-audio-receipt")?;
    let artifact = root.join("synthetic-package");
    fs::write(&artifact, b"unit fixture; not packaged playback")?;
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => return Err("unsupported test arch".into()),
    };
    let commit = "0123456789012345678901234567890123456789";
    let directory = root.join("bound");
    audio_package::bind(
        &std::env::current_exe()?,
        &artifact,
        commit,
        arch,
        &directory,
    )?;
    audio_package::validate(&directory, &artifact, commit, arch)?;
    assert!(audio_package::validate(
        &directory,
        &artifact,
        "1123456789012345678901234567890123456789",
        arch
    )
    .is_err());
    assert!(audio_package::validate(
        &directory,
        &artifact,
        commit,
        if arch == "amd64" { "arm64" } else { "amd64" }
    )
    .is_err());
    fs::write(&artifact, b"different artifact")?;
    assert!(audio_package::validate(&directory, &artifact, commit, arch).is_err());
    fs::write(&artifact, b"unit fixture; not packaged playback")?;
    fs::write(directory.join("audio-probe"), b"tampered executable")?;
    assert!(audio_package::validate(&directory, &artifact, commit, arch).is_err());
    fs::remove_dir_all(root)?;
    Ok(())
}

// Under a CI checkout the long retirement scenario names made the server
// socket 109 bytes; PulseAudio truncated it and readiness timed out. The
// harness must refuse such a directory instead of waiting for a socket that
// can never appear, and accept the longest path that still fits.
#[test]
fn private_server_socket_must_fit_the_unix_path_limit() -> Result<()> {
    let socket_suffix = "/runtime/pulse.sock".len();
    let fits = format!("/{}", "a".repeat(107 - socket_suffix - 1));
    assert_eq!(
        audio_capture::owned_socket(std::path::Path::new(&fits))?
            .as_os_str()
            .len(),
        107
    );
    let historical = std::path::Path::new(
        "/__w/among/among/audio-package-evidence/retirement/cancel-late-spawn-event-revoke-recovery",
    );
    assert!(audio_capture::owned_socket(historical).is_err());
    Ok(())
}

// A decoder that negotiated shared memory with PulseAudio maps libpulse's
// memfd ("pulseaudio") or POSIX ("/pulse-shm-N") pool; that client can make a
// PulseAudio server recording the sink monitor abort when it disconnects.
#[test]
fn decoder_maps_reveal_a_shared_memory_pulse_connection() -> Result<()> {
    let copied = "7f00-7f10 r-xp 0 00:1 2 /payload/usr/lib/libpulse.so.0\n7f20-7f30 rw-s 0 00:1 3 /memfd:among-sound (deleted)\n";
    audio_package::shared_pulse_memory_absent(copied)?;
    let memfd = format!("{copied}7f40-8f40 rw-s 0 00:1 4 /memfd:pulseaudio (deleted)\n");
    assert!(audio_package::shared_pulse_memory_absent(&memfd).is_err());
    let posix = format!("{copied}7f40-8f40 rw-s 0 00:1 4 /dev/shm/pulse-shm-1234\n");
    assert!(audio_package::shared_pulse_memory_absent(&posix).is_err());
    Ok(())
}
