use crate::{
    action_review::{digest, new_output, read, write_report, Runner},
    storage, Result,
};
use serde_json::{json, Value};
use std::{collections::BTreeSet, fs, path::Path};

pub fn frames(info: &Value) -> Result<u64> {
    let streams = info["streams"]
        .as_array()
        .ok_or("source stream inventory absent")?;
    if streams.len() != 1
        || streams[0]["codec_type"] != "video"
        || streams[0]["width"] != 1600
        || streams[0]["height"] != 1200
        || streams[0]["r_frame_rate"] != "25/1"
    {
        return Err("approved source stream/geometry/rate mismatch".into());
    }
    streams[0]["nb_frames"]
        .as_str()
        .ok_or("source frame count absent")?
        .parse::<u64>()?
        .checked_sub(1)
        .and_then(|frames| frames.checked_add(1))
        .filter(|frames| *frames <= 1_000_000)
        .ok_or_else(|| "invalid source frame count".into())
}
pub fn timeline(manifest: &Value, counts: &[u64]) -> Result<Value> {
    let entries = manifest["files"]
        .as_array()
        .ok_or("approved source manifest absent")?;
    if entries.len() != 13 || counts.len() != entries.len() {
        return Err("preparation requires all thirteen source clips".into());
    }
    let mut names = BTreeSet::new();
    let mut cursor = 125u64;
    let mut shots = Vec::new();
    for (entry, frames) in entries.iter().zip(counts) {
        let name = entry["name"]
            .as_str()
            .ok_or("approved source name absent")?;
        storage::relative_path(Path::new(name))?;
        let label = entry["title"]
            .as_str()
            .ok_or("approved source title absent")?;
        if Path::new(name).components().count() != 1
            || !names.insert(name)
            || *frames == 0
            || *frames > 1_000_000
        {
            return Err("unsafe duplicate source name or invalid frame count".into());
        }
        let hash = entry["sha256"].as_str().ok_or("source hash absent")?;
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err("invalid source digest".into());
        }
        shots.push(json!({"id":name,"label":label,"fromFrame":cursor,"duration":frames,"src":format!("{name}.mp4"),"sha256":hash}));
        cursor = cursor
            .checked_add(*frames)
            .ok_or("prepared timeline overflow")?;
    }
    let total = cursor.checked_add(75).ok_or("prepared outro overflow")?;
    if total > 1_000_000 {
        return Err("prepared movie exceeds bounded review frame limit".into());
    }
    Ok(
        json!({"fps":25,"width":1920,"height":1440,"intro":125,"shots":shots,"outroFrom":cursor,"outro":75,"total":total}),
    )
}
pub fn prepare(root: &Path, output: &Path) -> Result<Value> {
    let root = root.canonicalize()?;
    let manifest = read(&root, "docs/media/features/manifest.json")?;
    let entries = manifest["files"]
        .as_array()
        .ok_or("approved source manifest absent")?;
    // Validate names and the complete manifest before any source path is used.
    timeline(&manifest, &vec![1; entries.len()])?;
    let mut clips = Vec::new();
    for entry in entries {
        let name = entry["name"].as_str().ok_or("source name absent")?;
        let path =
            storage::safe_path(&root, Path::new(&format!("docs/media/features/{name}.mp4")))?;
        if entry["sha256"].as_str() != Some(digest(&path)?.as_str()) {
            return Err("source is not the approved clip".into());
        }
        clips.push(path);
    }
    let hero = entries
        .iter()
        .find(|entry| entry["name"] == "02-split-resize")
        .ok_or("approved hero clip absent")?;
    let poster = storage::safe_path(&root, Path::new("docs/media/features/02-split-resize.png"))?;
    let poster_hash = digest(&poster)?;
    if hero["poster_sha256"].as_str() != Some(poster_hash.as_str()) {
        return Err("unreviewed hero poster".into());
    }
    let output = new_output(&root, output)?;
    let mut runner = Runner::new(&output)?;
    let mut counts = Vec::new();
    for clip in &clips {
        let info = runner.command(
            "/usr/bin/ffprobe",
            &[
                "-v",
                "error",
                "-show_streams",
                "-of",
                "json",
                clip.to_str().ok_or("clip path is not UTF-8")?,
            ]
            .map(String::from),
        )?;
        counts.push(frames(&storage::parse_json(&info)?)?);
    }
    let timeline = timeline(&manifest, &counts)?;
    let public = output.join("public");
    let source = output.join("src");
    fs::create_dir(&public)?;
    fs::create_dir(&source)?;
    for (entry, clip) in entries.iter().zip(&clips) {
        let destination = public.join(format!(
            "{}.mp4",
            entry["name"].as_str().ok_or("clip name absent")?
        ));
        fs::copy(clip, &destination)?;
        if digest(&destination)? != entry["sha256"].as_str().ok_or("clip digest absent")? {
            return Err("source changed while copying approved clip".into());
        }
    }
    fs::copy(&poster, public.join("hero.png"))?;
    if digest(&public.join("hero.png"))? != poster_hash {
        return Err("hero poster changed while copying".into());
    }
    let last = public.join(
        clips
            .last()
            .ok_or("last source clip absent")?
            .file_name()
            .ok_or("last source filename absent")?,
    );
    let filter = format!(
        "select=eq(n\\,{})",
        counts.last().ok_or("last source frame count absent")? - 1
    );
    let end = public.join("end.png");
    runner.command(
        "/usr/bin/ffmpeg",
        &[
            "-v",
            "error",
            "-threads",
            "1",
            "-i",
            last.to_str().ok_or("last source path is not UTF-8")?,
            "-vf",
            &filter,
            "-frames:v",
            "1",
            "-threads",
            "1",
            end.to_str().ok_or("end frame path is not UTF-8")?,
        ]
        .map(String::from),
    )?;
    if fs::metadata(&end)?.len() == 0 {
        return Err("last workflow frame was not decoded".into());
    }
    let font = runner.command(
        "/usr/bin/fc-match",
        &["-f", "%{file}", "sans-serif"].map(String::from),
    )?;
    let font = String::from_utf8(font)?;
    let font = Path::new(&font);
    let font_hash = digest(font)?;
    fs::copy(font, public.join("brand.ttf"))?;
    if digest(&public.join("brand.ttf"))? != font_hash {
        return Err("resolved font changed while freezing".into());
    }
    if read(&root, "docs/media/features/manifest.json")? != manifest
        || digest(&poster)? != poster_hash
    {
        return Err("approved preparation inputs changed".into());
    }
    for (entry, clip) in entries.iter().zip(clips) {
        if digest(&clip)? != entry["sha256"].as_str().ok_or("source hash absent")? {
            return Err("approved source changed during preparation".into());
        }
    }
    write_report(&source, "timeline.json", &timeline)?;
    let report = json!({"duration":timeline["total"].as_u64().ok_or("timeline total absent")? as f64/25.0,"source_count":entries.len(),"total":timeline["total"]});
    write_report(&output, "prepare-summary.json", &report)?;
    write_report(
        &output,
        "prepared-assets.json",
        &json!({"timeline":digest(&source.join("timeline.json"))?,"hero":poster_hash,"end":digest(&end)?,"brand":font_hash}),
    )?;
    Ok(report)
}
