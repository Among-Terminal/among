use crate::{
    action_review::{digest, new_output, read, write_report, Runner},
    storage, Result,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

fn number(value: &Value, key: &str) -> Result<u64> {
    value[key]
        .as_u64()
        .filter(|number| *number > 0)
        .ok_or_else(|| format!("invalid positive timeline field: {key}").into())
}
pub fn selected_frames(timeline: &Value) -> Result<Vec<u64>> {
    let total = number(timeline, "total")?;
    if total > 1_000_000
        || number(timeline, "width")? > 8192
        || number(timeline, "height")? > 8192
        || number(timeline, "fps")? > 240
    {
        return Err("timeline exceeds review bounds".into());
    }
    let outro = number(timeline, "outroFrom")?;
    if outro >= total {
        return Err("outro boundary outside encoded movie".into());
    }
    let shots = timeline["shots"]
        .as_array()
        .ok_or("timeline shots absent")?;
    if shots.len() != 13 {
        return Err("long promo must retain all thirteen approved clips".into());
    }
    let mut cursor = number(timeline, "intro")?;
    let mut frames = BTreeSet::from([0, 25, 75, 99, 124, outro, total - 1]);
    for shot in shots {
        let start = number(shot, "fromFrame")?;
        let duration = number(shot, "duration")?;
        if start != cursor {
            return Err("timeline clips are not contiguous".into());
        }
        cursor = start
            .checked_add(duration)
            .ok_or("timeline frame overflow")?;
        if cursor > outro {
            return Err("clip exceeds outro boundary".into());
        }
        frames.extend([
            start - 1,
            start,
            start + 25.min(duration - 1),
            start + duration / 2,
            cursor - 1,
        ]);
    }
    if cursor != outro
        || outro
            .checked_add(number(timeline, "outro")?)
            .ok_or("outro frame overflow")?
            != total
        || frames.iter().any(|frame| *frame >= total)
    {
        return Err("timeline total or selected frame boundary mismatch".into());
    }
    Ok(frames.into_iter().collect())
}
pub fn validate_media(info: &Value, timeline: &Value) -> Result<f64> {
    selected_frames(timeline)?;
    let streams = info["streams"]
        .as_array()
        .ok_or("encoded stream inventory absent")?;
    if streams.len() != 1
        || streams[0]["codec_type"] != "video"
        || streams[0]["width"] != timeline["width"]
        || streams[0]["height"] != timeline["height"]
        || streams[0]["nb_frames"]
            .as_str()
            .ok_or("encoded frame count absent")?
            .parse::<u64>()?
            != number(timeline, "total")?
    {
        return Err("long promo stream/geometry/frame count mismatch".into());
    }
    let duration = info["format"]["duration"]
        .as_str()
        .ok_or("encoded duration absent")?
        .parse::<f64>()?;
    if !duration.is_finite() || duration <= 0.0 {
        return Err("encoded duration must be finite and positive".into());
    }
    Ok(duration)
}
pub fn validate_sources(root: &Path, timeline: &Value) -> Result<Value> {
    selected_frames(timeline)?;
    let manifest = read(root, "docs/media/features/manifest.json")?;
    let entries = manifest["files"]
        .as_array()
        .ok_or("feature manifest absent")?;
    if entries.len() != 13 {
        return Err("original gallery must retain thirteen approved clips".into());
    }
    let mut hashes = BTreeMap::new();
    for entry in entries {
        let name = entry["name"].as_str().ok_or("feature name absent")?;
        storage::relative_path(Path::new(name))?;
        if Path::new(name).components().count() != 1 {
            return Err("unsafe feature name".into());
        }
        let path = storage::safe_path(root, Path::new(&format!("docs/media/features/{name}.mp4")))?;
        let actual = digest(&path)?;
        if entry["sha256"].as_str() != Some(actual.as_str())
            || hashes.insert(name.to_string(), actual).is_some()
        {
            return Err("changed or duplicate original gallery source".into());
        }
    }
    let mut ids = BTreeSet::new();
    for shot in timeline["shots"]
        .as_array()
        .ok_or("timeline shots absent")?
    {
        let id = shot["id"].as_str().ok_or("timeline clip identity absent")?;
        if !ids.insert(id)
            || shot["sha256"].as_str() != hashes.get(id).map(String::as_str)
            || shot["src"] != format!("{id}.mp4")
        {
            return Err("timeline is not bound to original gallery bytes".into());
        }
    }
    let overview = read(root, "docs/media/overview-manifest.json")?;
    let actual = digest(&storage::safe_path(
        root,
        Path::new("docs/media/among-terminal-overview.mp4"),
    )?)?;
    if overview["sha256"].as_str() != Some(actual.as_str()) {
        return Err("original overview bytes changed".into());
    }
    Ok(json!({"gallery":hashes,"overview":actual}))
}

pub fn review(root: &Path, filename: &str, output: &Path) -> Result<Value> {
    storage::relative_path(Path::new(filename))?;
    if Path::new(filename).components().count() != 1 || !filename.ends_with(".mp4") {
        return Err("review video must be a safe MP4 basename".into());
    }
    let root = root.canonicalize()?;
    let timeline = read(&root, "video/readme-promo/src/timeline.json")?;
    let frames = selected_frames(&timeline)?;
    let sources = validate_sources(&root, &timeline)?;
    let video = storage::safe_path(
        &root,
        Path::new(&format!("video/readme-promo/out/{filename}")),
    )?;
    let hash = digest(&video)?;
    let reference = storage::safe_path(
        &root,
        Path::new("video/readme-promo/out/gallery-slow-push-in.mp4"),
    )?;
    let reference_hash = digest(&reference)?;
    let output = new_output(&root, output)?;
    let mut runner = Runner::new(&output)?;
    let video_path = video.to_str().ok_or("video path is not UTF-8")?;
    let info = runner.command(
        "/usr/bin/ffprobe",
        &[
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
            video_path,
        ]
        .map(String::from),
    )?;
    let duration = validate_media(&storage::parse_json(&info)?, &timeline)?;
    runner.command(
        "/usr/bin/ffmpeg",
        &[
            "-v", "error", "-threads", "1", "-i", video_path, "-xerror", "-f", "null", "-",
        ]
        .map(String::from),
    )?;
    let expression = frames
        .iter()
        .map(|frame| format!("eq(n,{frame})"))
        .collect::<Vec<_>>()
        .join("+");
    let filter = format!("select='{expression}'");
    let keys = output.join("key-%03d.png");
    runner.command(
        "/usr/bin/ffmpeg",
        &[
            "-v",
            "error",
            "-threads",
            "1",
            "-i",
            video_path,
            "-vf",
            &filter,
            "-fps_mode",
            "vfr",
            "-threads",
            "1",
            keys.to_str().ok_or("keyframe path is not UTF-8")?,
        ]
        .map(String::from),
    )?;
    let mut keyframes = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        let name = format!("key-{:03}.png", index + 1);
        if fs::metadata(output.join(&name))?.len() == 0 {
            return Err("selected boundary frame was not decoded".into());
        }
        keyframes.push(json!({"frame":frame,"file":name}));
    }
    let sheets = output.join("timeline-%02d.jpg");
    runner.command("/usr/bin/ffmpeg",&["-v","error","-threads","1","-i",video_path,"-vf","fps=2,scale=320:240,drawtext=text='%{pts\\:hms}':fontcolor=white:fontsize=16:box=1:boxcolor=black@0.8,tile=4x5","-fps_mode","vfr","-threads","1",sheets.to_str().ok_or("timeline path is not UTF-8")?].map(String::from))?;
    let preview = output.join("gallery-reference.jpg");
    runner.command(
        "/usr/bin/ffmpeg",
        &[
            "-v",
            "error",
            "-threads",
            "1",
            "-i",
            reference.to_str().ok_or("reference path is not UTF-8")?,
            "-vf",
            "fps=2,scale=320:180,tile=4x3",
            "-frames:v",
            "1",
            "-threads",
            "1",
            preview
                .to_str()
                .ok_or("reference preview path is not UTF-8")?,
        ]
        .map(String::from),
    )?;
    if fs::metadata(preview)?.len() == 0
        || !fs::read_dir(&output)?.any(|entry| {
            entry.is_ok_and(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("timeline-") && name.ends_with(".jpg"))
                    && entry.metadata().is_ok_and(|metadata| metadata.len() > 0)
            })
        })
    {
        return Err("review contact sheets were not decoded".into());
    }
    if digest(&video)? != hash
        || digest(&reference)? != reference_hash
        || read(&root, "video/readme-promo/src/timeline.json")? != timeline
        || validate_sources(&root, &timeline)? != sources
    {
        return Err("long promo review inputs changed during decode".into());
    }
    let report = json!({"duration":duration,"frames":timeline["total"],"width":timeline["width"],"height":timeline["height"],"fps":timeline["fps"],"sha256":hash,"bytes":fs::metadata(video)?.len(),"full_decode":true,"audio_streams":0,"subtitle_streams":0,"original_gallery_and_overview_unchanged":true,"source_clips":13,"keyframes":keyframes,"visual_review":"pending","browser_playback":"pending","independent_review":"pending"});
    write_report(&output, "keyframes.json", &json!(keyframes))?;
    write_report(&output, "promo-manifest.json", &report)?;
    Ok(report)
}
