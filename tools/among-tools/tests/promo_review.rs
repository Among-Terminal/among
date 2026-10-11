use among_tools::{promo_review, storage, Result};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn timeline() -> Value {
    let shots=(0..13).map(|index|json!({"id":format!("clip-{index}"),"src":format!("clip-{index}.mp4"),"sha256":"fixture","fromFrame":125+index*30,"duration":30})).collect::<Vec<_>>();
    json!({"fps":25,"width":1920,"height":1440,"intro":125,"shots":shots,"outroFrom":515,"outro":75,"total":590})
}
fn fixture() -> Result<(Fixture, Value)> {
    let f = Fixture(storage::private_directory(
        &std::env::temp_dir(),
        "among-promo-review",
    )?);
    fs::create_dir_all(f.0.join("docs/media/features"))?;
    let mut timeline = timeline();
    let mut files = Vec::new();
    for index in 0..13 {
        let name = format!("clip-{index}");
        let path = f.0.join(format!("docs/media/features/{name}.mp4"));
        fs::write(
            &path,
            format!("synthetic source bytes {index}, not encoded media"),
        )?;
        let hash = storage::sha256(fs::File::open(path)?)?;
        timeline["shots"][index]["sha256"] = json!(hash);
        files.push(json!({"name":name,"sha256":hash}));
    }
    fs::write(
        f.0.join("docs/media/features/manifest.json"),
        serde_json::to_vec(&json!({"files":files}))?,
    )?;
    let overview = f.0.join("docs/media/among-terminal-overview.mp4");
    fs::write(&overview, b"synthetic overview")?;
    fs::write(
        f.0.join("docs/media/overview-manifest.json"),
        serde_json::to_vec(&json!({"sha256":storage::sha256(fs::File::open(overview)?)?}))?,
    )?;
    Ok((f, timeline))
}
#[test]
fn boundary_selection_retains_original_intro_shot_and_fade_formula_without_overflow() -> Result<()>
{
    let timeline = timeline();
    let selected = promo_review::selected_frames(&timeline)?;
    for frame in [0, 25, 75, 99, 124, 125, 150, 140, 154, 514, 515, 589] {
        assert!(
            selected.contains(&frame),
            "missing original boundary: {frame}"
        );
    }
    assert!(selected.windows(2).all(|pair| pair[0] < pair[1]));
    for field in [
        "total",
        "width",
        "height",
        "fps",
        "intro",
        "outroFrom",
        "outro",
    ] {
        let mut changed = timeline.clone();
        changed[field] = json!(0);
        assert!(promo_review::selected_frames(&changed).is_err());
    }
    let mut gap = timeline.clone();
    gap["shots"][1]["fromFrame"] = json!(156);
    assert!(promo_review::selected_frames(&gap).is_err());
    let mut missing = timeline.clone();
    missing["shots"].as_array_mut().ok_or("shots absent")?.pop();
    assert!(promo_review::selected_frames(&missing).is_err());
    let mut overflow = timeline.clone();
    overflow["shots"][0]["duration"] = json!(u64::MAX);
    assert!(promo_review::selected_frames(&overflow).is_err());
    Ok(())
}
#[test]
fn encoded_metadata_and_complete_original_source_timeline_binding_refuse_changes() -> Result<()> {
    let (f, timeline) = fixture()?;
    promo_review::validate_sources(&f.0, &timeline)?;
    let info = json!({"streams":[{"codec_type":"video","width":1920,"height":1440,"nb_frames":"590"}],"format":{"duration":"23.6"}});
    assert_eq!(promo_review::validate_media(&info, &timeline)?, 23.6);
    for field in ["codec_type", "width", "height", "nb_frames"] {
        let mut changed = info.clone();
        changed["streams"][0][field] = json!(null);
        assert!(promo_review::validate_media(&changed, &timeline).is_err());
    }
    let mut audio = info.clone();
    audio["streams"]
        .as_array_mut()
        .ok_or("streams absent")?
        .push(json!({"codec_type":"audio"}));
    assert!(promo_review::validate_media(&audio, &timeline).is_err());
    for field in ["id", "src", "sha256"] {
        let mut changed = timeline.clone();
        changed["shots"][0][field] = json!("changed");
        assert!(promo_review::validate_sources(&f.0, &changed).is_err());
    }
    fs::write(f.0.join("docs/media/features/clip-0.mp4"), b"changed")?;
    assert!(promo_review::validate_sources(&f.0, &timeline).is_err());
    Ok(())
}
#[test]
fn actual_cli_refuses_unsafe_filename_before_any_review_output() -> Result<()> {
    let (f, _timeline) = fixture()?;
    let output = f.0.join("must-not-exist");
    for filename in [
        "../outside.mp4",
        "/outside.mp4",
        "nested/video.mp4",
        "video.txt",
    ] {
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_among-tools"))
            .env_clear()
            .arg("review-readme-promo")
            .arg(&f.0)
            .arg(filename)
            .arg(&output)
            .output()?;
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
        assert!(!output.exists());
    }
    Ok(())
}
