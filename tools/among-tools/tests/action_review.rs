use among_tools::{action_review, storage, Result};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn fixture() -> Result<Fixture> {
    let f = Fixture(storage::private_directory(
        &std::env::temp_dir(),
        "among-action-review",
    )?);
    fs::create_dir_all(f.0.join("docs/media/features"))?;
    fs::create_dir_all(f.0.join("video/readme-promo/public"))?;
    fs::create_dir_all(f.0.join("video/readme-promo/out"))?;
    for name in ["clip", "plugins-on"] {
        fs::write(
            f.0.join(format!("video/readme-promo/public/{name}.mp4")),
            b"synthetic source-byte fixture, not encoded media",
        )?;
    }
    let hash = storage::sha256(fs::File::open(
        f.0.join("video/readme-promo/public/clip.mp4"),
    )?)?;
    fs::write(
        f.0.join("docs/media/features/manifest.json"),
        serde_json::to_vec(&json!({"files":[{"name":"clip","sha256":hash}]}))?,
    )?;
    write_plugin(
        &f,
        &json!({"sha256":hash,"events":[{"enabled":["clock","system-status","pane-pwd"]}],"observed_pane_directories":["one","two","three"]}),
    )?;
    fs::write(
        f.0.join("video/readme-promo/out/action-v5.mp4"),
        b"synthetic invalid encoded input for refusal tests",
    )?;
    Ok(f)
}
fn write_plugin(f: &Fixture, value: &Value) -> Result<()> {
    fs::write(
        f.0.join("video/readme-promo/out/plugins-on-source.json"),
        serde_json::to_vec(value)?,
    )?;
    Ok(())
}

#[test]
fn encoded_action_metadata_retains_all_original_geometry_and_no_audio_assertions() -> Result<()> {
    let info = json!({"streams":[{"codec_type":"video","width":1920,"height":1440,"nb_frames":"575"}],"format":{"duration":"23.0"}});
    assert_eq!(action_review::validate_media(&info)?, 23.0);
    for field in ["codec_type", "width", "height", "nb_frames"] {
        let mut altered = info.clone();
        altered["streams"][0][field] = json!(null);
        assert!(action_review::validate_media(&altered).is_err());
    }
    let mut audio = info.clone();
    audio["streams"]
        .as_array_mut()
        .ok_or("streams absent")?
        .push(json!({"codec_type":"audio"}));
    assert!(action_review::validate_media(&audio).is_err());
    for duration in ["NaN", "inf", "0", "-1"] {
        let mut altered = info.clone();
        altered["format"]["duration"] = json!(duration);
        assert!(action_review::validate_media(&altered).is_err());
    }
    assert_eq!(action_review::FRAMES.len(), 29);
    assert_eq!(action_review::FRAMES[28], 574);
    Ok(())
}
#[test]
fn approved_source_and_plugin_bytes_events_directories_and_safe_paths_are_bound() -> Result<()> {
    let f = fixture()?;
    action_review::validate_sources(&f.0)?;
    let plugin_path = f.0.join("video/readme-promo/out/plugins-on-source.json");
    let plugin = storage::parse_json(&fs::read(&plugin_path)?)?;
    for field in ["sha256", "events", "observed_pane_directories"] {
        let mut changed = plugin.clone();
        changed[field] = json!(null);
        write_plugin(&f, &changed)?;
        assert!(action_review::validate_sources(&f.0).is_err());
    }
    let mut repeated = plugin.clone();
    repeated["observed_pane_directories"] = json!(["one", "one", "two"]);
    write_plugin(&f, &repeated)?;
    assert!(action_review::validate_sources(&f.0).is_err());
    let mut unexpected = plugin.clone();
    unexpected["events"][0]["enabled"] = json!(["clock"]);
    write_plugin(&f, &unexpected)?;
    assert!(action_review::validate_sources(&f.0).is_err());
    write_plugin(&f, &plugin)?;
    fs::write(f.0.join("video/readme-promo/public/clip.mp4"), b"changed")?;
    assert!(action_review::validate_sources(&f.0).is_err());
    let mut manifest =
        storage::parse_json(&fs::read(f.0.join("docs/media/features/manifest.json"))?)?;
    manifest["files"][0]["name"] = json!("../outside");
    fs::write(
        f.0.join("docs/media/features/manifest.json"),
        serde_json::to_vec(&manifest)?,
    )?;
    assert!(action_review::validate_sources(&f.0).is_err());
    Ok(())
}
#[test]
fn actual_cli_refuses_workspace_or_changed_sources_without_touching_approved_delivery() -> Result<()>
{
    let f = fixture()?;
    let video = f.0.join("video/readme-promo/out/action-v5.mp4");
    let bytes = fs::read(&video)?;
    let inside = f.0.join("video/readme-promo/out/new-qa");
    let invoke = |output: &std::path::Path| {
        std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .arg("review-readme-action")
            .arg(&f.0)
            .arg(output)
            .output()
    };
    let result = invoke(&inside)?;
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(!inside.exists());
    fs::write(
        f.0.join("video/readme-promo/public/plugins-on.mp4"),
        b"modified",
    )?;
    let outside = f.0.join("new-qa");
    assert!(!invoke(&outside)?.status.success());
    assert!(!outside.exists());
    assert_eq!(fs::read(video)?, bytes);
    Ok(())
}
