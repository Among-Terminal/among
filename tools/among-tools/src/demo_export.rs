use crate::{
    action_review::{digest, new_output, read, write_report, Runner},
    storage, Result,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

pub fn stamp(seconds: f64) -> Result<String> {
    if !seconds.is_finite() || !(0.0..=86_400.0).contains(&seconds) {
        return Err("invalid caption timestamp".into());
    }
    let milliseconds = (seconds * 1000.0).round_ties_even() as u64;
    Ok(format!(
        "{:02}:{:02}:{:02},{:03}",
        milliseconds / 3_600_000,
        milliseconds / 60_000 % 60,
        milliseconds / 1000 % 60,
        milliseconds % 1000
    ))
}

pub fn captions(record: &Value) -> Result<(String, String)> {
    let duration = record["duration"]
        .as_f64()
        .filter(|v| v.is_finite() && (60.0..=90.0).contains(v))
        .ok_or("public demonstration must be 60–90 seconds")?;
    let events = record["events"]
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or("caption events absent")?;
    let mut captions = Vec::new();
    let mut transcript = vec![
        "# Among Terminal demonstration".to_string(),
        String::new(),
        "Silent real UI recording with English captions.".into(),
        String::new(),
    ];
    for (index, event) in events.iter().enumerate() {
        let start = event["start"]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0 && *v < duration)
            .ok_or("invalid caption timing")?;
        let end = if index + 1 < events.len() {
            events[index + 1]["start"]
                .as_f64()
                .ok_or("next caption timestamp absent")?
        } else {
            duration
        };
        if !end.is_finite() || end <= start || end > duration {
            return Err("caption events must be ordered".into());
        }
        let text = event["caption"].as_str().ok_or("caption text absent")?;
        captions.push(format!(
            "{}\n{} --> {}\n{text}\n",
            index + 1,
            stamp(start)?,
            stamp(end)?
        ));
        transcript.push(format!("- {start:.1}s: {text}"));
    }
    Ok((captions.join("\n"), format!("{}\n", transcript.join("\n"))))
}

pub fn validate_media(media: &Value) -> Result<()> {
    let streams = media["streams"]
        .as_array()
        .ok_or("exported stream inventory absent")?;
    if streams.len() != 1
        || streams[0]["codec_type"] != "video"
        || streams[0]["codec_name"] != "h264"
        || streams[0]["pix_fmt"] != "yuv420p"
    {
        return Err("unexpected codec/pixel format or extra media stream".into());
    }
    let duration = media["format"]["duration"]
        .as_str()
        .ok_or("exported duration absent")?
        .parse::<f64>()?;
    if !duration.is_finite() || !(60.0..=90.0).contains(&duration) {
        return Err("encoded duration violates storyboard contract".into());
    }
    let allowed = BTreeSet::from([
        "major_brand",
        "minor_version",
        "compatible_brands",
        "encoder",
        "language",
        "handler_name",
        "vendor_id",
    ]);
    for metadata in std::iter::once(&media["format"]).chain(streams.iter()) {
        if let Some(tags) = metadata.get("tags") {
            if tags
                .as_object()
                .ok_or("exported tags must be an object")?
                .keys()
                .any(|key| !allowed.contains(key.as_str()))
            {
                return Err("unexpected exported metadata needs review".into());
            }
        }
    }
    Ok(())
}

fn text(path: &Path, content: &str) -> Result<()> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

pub fn export(source: &Path, output: &Path) -> Result<Value> {
    let source = source.canonicalize()?;
    let record = read(&source, "storyboard-result.json")?;
    let (srt, transcript) = captions(&record)?;
    let raw = storage::safe_path(&source, Path::new("raw.mp4"))?;
    let original = digest(&raw)?;
    if output
        .parent()
        .ok_or("export output parent absent")?
        .canonicalize()?
        .starts_with(&source)
    {
        return Err("export output must be outside original capture inputs".into());
    }
    let output = new_output(&source, output)?;
    fs::copy(&raw, output.join("raw.mp4"))?;
    if digest(&output.join("raw.mp4"))? != original {
        return Err("capture changed during private copy".into());
    }
    text(&output.join("captions.srt"), &srt)?;
    text(&output.join("transcript.md"), &transcript)?;
    let mut runner = Runner::new(&output)?;
    runner.command("/usr/bin/ffmpeg", &["-hide_banner", "-loglevel", "error", "-threads", "1", "-i", "raw.mp4", "-vf", "pad=iw:ih+160:0:0:color=black,subtitles=captions.srt:force_style='FontName=DejaVu Sans,FontSize=9,Alignment=2,MarginV=8,Outline=1'", "-an", "-map_metadata", "-1", "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p", "-movflags", "+faststart", "among-terminal-demo.mp4"].map(String::from))?;
    let info = runner.command(
        "/usr/bin/ffprobe",
        &[
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
            "among-terminal-demo.mp4",
        ]
        .map(String::from),
    )?;
    let media = storage::parse_json(&info)?;
    validate_media(&media)?;
    runner.command(
        "/usr/bin/ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-threads",
            "1",
            "-ss",
            "38",
            "-i",
            "among-terminal-demo.mp4",
            "-frames:v",
            "1",
            "-threads",
            "1",
            "poster.png",
        ]
        .map(String::from),
    )?;
    fs::create_dir(output.join("review-frames"))?;
    runner.command(
        "/usr/bin/ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-threads",
            "1",
            "-i",
            "among-terminal-demo.mp4",
            "-vf",
            "fps=1,scale=800:-1",
            "-threads",
            "1",
            "review-frames/%03d.png",
        ]
        .map(String::from),
    )?;
    if fs::metadata(output.join("poster.png"))?.len() == 0
        || fs::read_dir(output.join("review-frames"))?.next().is_none()
    {
        return Err("export review frames absent".into());
    }
    if read(&source, "storyboard-result.json")? != record
        || digest(&raw)? != original
        || digest(&output.join("raw.mp4"))? != original
        || fs::read_to_string(output.join("captions.srt"))? != srt
        || fs::read_to_string(output.join("transcript.md"))? != transcript
    {
        return Err("export capture/caption inputs changed".into());
    }
    let report = json!({"schema":1, "media":media, "privacy_review":"pending full-frame/video review; no public README embedding yet"});
    write_report(&output, "media-inspection.json", &report)?;
    Ok(report)
}
