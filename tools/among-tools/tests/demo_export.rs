use among_tools::{demo_export, storage, Result};
use serde_json::json;
use std::fs;

#[test]
fn original_caption_timecodes_order_and_boundaries_are_preserved() -> Result<()> {
    for (seconds, expected) in [
        (0.0, "00:00:00,000"),
        (65.125, "00:01:05,125"),
        (3599.999, "00:59:59,999"),
        (0.0005, "00:00:00,000"),
        (0.0015, "00:00:00,002"),
    ] {
        assert_eq!(demo_export::stamp(seconds)?, expected);
    }
    for value in [f64::NAN, f64::INFINITY, -1.0] {
        assert!(demo_export::stamp(value).is_err());
    }
    let record = json!({"duration":80,"events":[{"start":0,"caption":"Fixture caption one"},{"start":65.125,"caption":"Fixture caption two"}]});
    let (captions, transcript) = demo_export::captions(&record)?;
    assert_eq!(captions,"1\n00:00:00,000 --> 00:01:05,125\nFixture caption one\n\n2\n00:01:05,125 --> 00:01:20,000\nFixture caption two\n");
    assert!(transcript.ends_with("- 65.1s: Fixture caption two\n"));
    for changed in [
        json!({"duration":10,"events":[]}),
        json!({"duration":80,"events":[]}),
        json!({"duration":80,"events":[{"start":10,"caption":"Split"},{"start":9,"caption":"Zoom"}]}),
        json!({"duration":80,"events":[{"start":80,"caption":"outside"}]}),
    ] {
        assert!(demo_export::captions(&changed).is_err());
    }
    Ok(())
}

#[test]
fn export_codec_silence_duration_and_privacy_metadata_are_actual_assertions() -> Result<()> {
    let media = json!({"streams":[{"codec_type":"video","codec_name":"h264","pix_fmt":"yuv420p","tags":{"language":"und"}}],"format":{"duration":"80.0","tags":{"major_brand":"isom"}}});
    demo_export::validate_media(&media)?;
    for field in ["codec_type", "codec_name", "pix_fmt"] {
        let mut changed = media.clone();
        changed["streams"][0][field] = json!("wrong");
        assert!(demo_export::validate_media(&changed).is_err());
    }
    for value in ["NaN", "59.9", "90.1"] {
        let mut changed = media.clone();
        changed["format"]["duration"] = json!(value);
        assert!(demo_export::validate_media(&changed).is_err());
    }
    let mut extra = media.clone();
    extra["streams"]
        .as_array_mut()
        .ok_or("streams absent")?
        .push(json!({"codec_type":"audio"}));
    assert!(demo_export::validate_media(&extra).is_err());
    for target in ["format", "streams"] {
        let mut changed = media.clone();
        if target == "format" {
            changed["format"]["tags"]["private-path"] = json!("fixture");
        } else {
            changed["streams"][0]["tags"]["private-path"] = json!("fixture");
        }
        assert!(demo_export::validate_media(&changed).is_err());
    }
    Ok(())
}

#[test]
fn actual_export_cli_refuses_invalid_storyboard_before_encoding_or_output_creation() -> Result<()> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-demo-export")?;
    fs::write(
        root.join("storyboard-result.json"),
        serde_json::to_vec(&json!({"duration":10,"events":[]}))?,
    )?;
    let output = root.join("must-not-exist");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("export-demo")
        .arg(&root)
        .arg(&output)
        .output()?;
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(!output.exists());
    fs::remove_dir_all(root)?;
    Ok(())
}
