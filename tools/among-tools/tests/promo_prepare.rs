use among_tools::{promo_prepare, storage, Result};
use serde_json::json;
use std::fs;

fn manifest() -> serde_json::Value {
    json!({"files":(0..13).map(|index|json!({"name":format!("clip-{index}"),"title":format!("Original title {index}"),"sha256":"a".repeat(64)})).collect::<Vec<_>>()})
}
#[test]
fn preparation_timeline_retains_original_frames_titles_and_intro_outro_without_hidden_retiming(
) -> Result<()> {
    let manifest = manifest();
    let counts = (1..=13).collect::<Vec<_>>();
    let timeline = promo_prepare::timeline(&manifest, &counts)?;
    assert_eq!(timeline["fps"], 25);
    assert_eq!(timeline["width"], 1920);
    assert_eq!(timeline["height"], 1440);
    let mut cursor = 125;
    for (index, count) in counts.iter().enumerate() {
        let shot = &timeline["shots"][index];
        assert_eq!(shot["fromFrame"], cursor);
        assert_eq!(shot["duration"], *count);
        assert_eq!(shot["label"], format!("Original title {index}"));
        assert_eq!(shot["sha256"], "a".repeat(64));
        cursor += count;
    }
    assert_eq!(timeline["outroFrom"], cursor);
    assert_eq!(timeline["total"], cursor + 75);
    assert!(promo_prepare::timeline(&manifest, &[1; 12]).is_err());
    assert!(promo_prepare::timeline(&manifest, &[0; 13]).is_err());
    assert!(promo_prepare::timeline(&manifest, &[u64::MAX; 13]).is_err());
    for field in ["name", "title", "sha256"] {
        let mut changed = manifest.clone();
        changed["files"][0][field] = json!(null);
        assert!(promo_prepare::timeline(&changed, &[1; 13]).is_err());
    }
    let mut unsafe_name = manifest.clone();
    unsafe_name["files"][0]["name"] = json!("../outside");
    assert!(promo_prepare::timeline(&unsafe_name, &[1; 13]).is_err());
    let mut duplicate = manifest;
    duplicate["files"][1]["name"] = duplicate["files"][0]["name"].clone();
    assert!(promo_prepare::timeline(&duplicate, &[1; 13]).is_err());
    Ok(())
}
#[test]
fn source_stream_validation_retains_geometry_rate_and_no_extra_streams() -> Result<()> {
    let info = json!({"streams":[{"codec_type":"video","width":1600,"height":1200,"r_frame_rate":"25/1","nb_frames":"352"}]});
    assert_eq!(promo_prepare::frames(&info)?, 352);
    for field in ["codec_type", "width", "height", "r_frame_rate", "nb_frames"] {
        let mut changed = info.clone();
        changed["streams"][0][field] = json!(null);
        assert!(promo_prepare::frames(&changed).is_err());
    }
    for count in ["0", "NaN", "1000001"] {
        let mut changed = info.clone();
        changed["streams"][0]["nb_frames"] = json!(count);
        assert!(promo_prepare::frames(&changed).is_err());
    }
    let mut extra = info;
    extra["streams"]
        .as_array_mut()
        .ok_or("streams absent")?
        .push(json!({"codec_type":"audio"}));
    assert!(promo_prepare::frames(&extra).is_err());
    Ok(())
}
#[test]
fn actual_preparation_cli_refuses_changed_source_before_creating_workspace() -> Result<()> {
    let root = storage::private_directory(&std::env::temp_dir(), "among-promo-prepare")?;
    fs::create_dir_all(root.join("docs/media/features"))?;
    fs::write(
        root.join("docs/media/features/manifest.json"),
        serde_json::to_vec(&manifest())?,
    )?;
    fs::write(
        root.join("docs/media/features/clip-0.mp4"),
        b"synthetic unapproved bytes",
    )?;
    let output = root.join("must-not-exist");
    let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
        .env_clear()
        .arg("prepare-readme-promo")
        .arg(&root)
        .arg(&output)
        .output()?;
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(!output.exists());
    assert_eq!(
        fs::read(root.join("docs/media/features/clip-0.mp4"))?,
        b"synthetic unapproved bytes"
    );
    fs::remove_dir_all(root)?;
    Ok(())
}
