use among_tools::demo_clip_media;
use among_tools::{demo_clips, storage, Result};
use serde_json::json;
use std::fs;

#[test]
fn original_shortcut_badges_are_centered_rounded_bold_nonoverlapping_and_absent_without_events(
) -> Result<()> {
    let parsed = demo_clip_media::events(
        &json!([{"start":1,"keys":"Alt+Right"},{"start":2,"keys":"Ctrl+Shift+X"}]),
    )?;
    let graph = demo_clip_media::graph(&parsed);
    assert!(graph[1].contains("x=(W-w)/2:y=(H-h)/2"));
    assert!(graph[1].contains("between(t,1,2)"));
    assert!(graph[1].contains("between(t,2,3.6)"));
    assert!(!graph[1].contains("subtitles"));
    let badge = demo_clip_media::badge_arguments(200, 40, "Alt+Right", "badge.png")?;
    for expected in [
        "DejaVu-Sans-Bold",
        "roundrectangle 0,0,259,75,18,18",
        "black",
    ] {
        assert!(badge.iter().any(|v| v == expected));
    }
    assert_eq!(
        demo_clip_media::graph(&demo_clip_media::events(&json!([]))?),
        vec!["-vf", "null"]
    );
    for bad in [
        json!([{"start":-1,"keys":"Alt+Right"}]),
        json!([{"start":1,"keys":"bad; command"}]),
        json!([{"start":2,"keys":"A"},{"start":1,"keys":"B"}]),
    ] {
        assert!(demo_clip_media::events(&bad).is_err());
    }
    Ok(())
}

#[test]
fn original_partial_take_directory_and_fresh_shell_assertions_remain() -> Result<()> {
    let record = json!({"clips":demo_clips::FEATURES.iter().map(|(name,_)|json!({"name":name})).collect::<Vec<_>>()});
    demo_clips::validate_record(&record)?;
    assert!(demo_clips::validate_record(&json!({"clips":[{"name":"01-theme-light"}]})).is_err());
    let proof = json!({"actual_directories":["atlas"],"saved_directories":["atlas"],"old_pids":["1"],"fresh_pids":["2"]});
    demo_clips::validate_proof(&proof)?;
    let mut wrong = proof.clone();
    wrong["actual_directories"] = json!(["wrong"]);
    assert!(demo_clips::validate_proof(&wrong).is_err());
    let mut reused = proof;
    reused["fresh_pids"] = json!(["1"]);
    assert!(demo_clips::validate_proof(&reused).is_err());
    Ok(())
}

#[test]
fn real_clip_metadata_contract_refuses_codec_geometry_duration_audio_and_private_tags() -> Result<()>
{
    let probe = json!({"streams":[{"codec_type":"video","codec_name":"h264","pix_fmt":"yuv420p","width":1600,"height":1200}],"format":{"duration":"14.08","tags":{"encoder":"fixture"}}});
    assert_eq!(demo_clips::validate_media(&probe)?, 14.08);
    for field in ["codec_type", "codec_name", "pix_fmt", "width", "height"] {
        let mut changed = probe.clone();
        changed["streams"][0][field] = json!(null);
        assert!(demo_clips::validate_media(&changed).is_err());
    }
    for duration in ["2.9", "60.1", "NaN"] {
        let mut changed = probe.clone();
        changed["format"]["duration"] = json!(duration);
        assert!(demo_clips::validate_media(&changed).is_err());
    }
    let mut audio = probe.clone();
    audio["streams"]
        .as_array_mut()
        .ok_or("streams absent")?
        .push(json!({"codec_type":"audio"}));
    assert!(demo_clips::validate_media(&audio).is_err());
    let mut private = probe;
    private["format"]["tags"]["private"] = json!("fixture");
    assert!(demo_clips::validate_media(&private).is_err());
    Ok(())
}

#[test]
fn actual_partial_capture_cli_never_creates_review_or_success_manifest() -> Result<()> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-demo-clips")?;
    fs::write(
        root.join("clips-result.json"),
        serde_json::to_vec(&json!({"clips":[{"name":"01-theme-light"}]}))?,
    )?;
    let output = root.join("must-not-exist");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("check-demo-clips")
        .arg(&root)
        .arg(&output)
        .output()?;
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(!output.exists());
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn actual_badge_encoder_refuses_invalid_events_and_symlink_inputs_before_output() -> Result<()> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-demo-badges")?;
    let raw = root.join("raw.mp4");
    let events = root.join("events.json");
    let output = root.join("must-not-exist");
    fs::write(&raw, b"synthetic input, not encoded media")?;
    fs::write(
        &events,
        serde_json::to_vec(&json!([{"start":1,"keys":"bad; command"}]))?,
    )?;
    let invoke = |raw: &std::path::Path| -> Result<()> {
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .arg("encode-demo-clip")
            .arg(raw)
            .arg(&events)
            .arg(&output)
            .output()?;
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(!output.exists());
        Ok(())
    };
    invoke(&raw)?;
    fs::write(&events, b"[]")?;
    let link = root.join("linked.mp4");
    std::os::unix::fs::symlink(&raw, &link)?;
    invoke(&link)?;
    fs::remove_dir_all(root)?;
    Ok(())
}
