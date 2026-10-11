use crate::{
    action_review::{digest, new_output, read, write_report, Runner},
    storage, Result,
};
use serde_json::{json, Value};
use std::{collections::BTreeSet, fs, path::Path};

pub const FEATURES: [(&str, &str); 13] = [
    ("01-themes", "Light and dark appearance"),
    ("02-split-resize", "Split terminals: ll, mc and htop"),
    ("02b-keyboard-resize", "Keyboard resize"),
    ("02c-mouse-resize", "Mouse divider resize"),
    ("03-pane-drag", "Pane header drag-and-drop"),
    ("04-focus", "Keyboard and mouse focus"),
    ("04b-zoom", "Pane zoom and restore"),
    ("04c-fullscreen", "Fullscreen and restore"),
    ("05-workspaces", "Create, rename and switch workspaces"),
    ("05b-sidebar", "Hide and show the sidebar"),
    ("06-plugins", "Enable and disable the three plugins"),
    (
        "07-session-restore",
        "Consented restoration of separate directories",
    ),
    (
        "08-close-pane",
        "Close a pane and type in the surviving sibling",
    ),
];

pub fn validate_record(record: &Value) -> Result<()> {
    let mut recorded = BTreeSet::new();
    for clip in record["clips"].as_array().ok_or("recorded clips absent")? {
        let name = clip["name"].as_str().ok_or("recorded clip name absent")?;
        recorded.insert(name);
    }
    if FEATURES
        .iter()
        .skip(1)
        .any(|(name, _)| !recorded.contains(name))
    {
        return Err("recording is incomplete; do not attach partial takes".into());
    }
    Ok(())
}

pub fn validate_proof(proof: &Value) -> Result<()> {
    let actual = proof["actual_directories"]
        .as_array()
        .ok_or("actual restoration directories absent")?;
    let saved = proof["saved_directories"]
        .as_array()
        .ok_or("saved restoration directories absent")?;
    if actual != saved {
        return Err("restoration evidence differs".into());
    }
    let old = proof["old_pids"]
        .as_array()
        .ok_or("old shell PIDs absent")?;
    let fresh = proof["fresh_pids"]
        .as_array()
        .ok_or("fresh shell PIDs absent")?;
    if old.iter().any(|pid| fresh.contains(pid)) {
        return Err("shells were not fresh".into());
    }
    Ok(())
}

pub fn validate_media(probe: &Value) -> Result<f64> {
    let streams = probe["streams"]
        .as_array()
        .ok_or("clip stream inventory absent")?;
    if streams.len() != 1
        || streams[0]["codec_type"] != "video"
        || streams[0]["codec_name"] != "h264"
        || streams[0]["pix_fmt"] != "yuv420p"
    {
        return Err("expected exactly one silent H.264/yuv420p video stream".into());
    }
    if streams[0]["width"] != 1600 || streams[0]["height"] != 1200 {
        return Err("unexpected capture size".into());
    }
    let duration = probe["format"]["duration"]
        .as_str()
        .ok_or("clip duration absent")?
        .parse::<f64>()?;
    if !duration.is_finite() || !(3.0..=60.0).contains(&duration) {
        return Err("clip timing is outside 3–60 seconds".into());
    }
    if let Some(tags) = probe["format"].get("tags") {
        let allowed = BTreeSet::from([
            "major_brand",
            "minor_version",
            "compatible_brands",
            "encoder",
        ]);
        if tags
            .as_object()
            .ok_or("clip tags must be an object")?
            .keys()
            .any(|key| !allowed.contains(key.as_str()))
        {
            return Err("unreviewed metadata tags".into());
        }
    }
    Ok(duration)
}

fn logs(source: &Path) -> Result<Vec<(std::path::PathBuf, String)>> {
    let mut result = Vec::new();
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err("capture filename is not UTF-8".into());
        };
        if name.starts_with("clips-app") && name.ends_with(".log") {
            let path = storage::safe_path(source, Path::new(name))?;
            let text = fs::read_to_string(&path)?;
            if text.contains("CRITICAL") || text.contains("panicked") {
                return Err("application failure in capture log".into());
            }
            result.push((path.clone(), digest(&path)?));
        }
    }
    result.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(result)
}

pub fn review(source: &Path, output: &Path) -> Result<Value> {
    let source = source.canonicalize()?;
    let record = read(&source, "clips-result.json")?;
    validate_record(&record)?;
    let proof = read(&source, "restoration-proof.json")?;
    validate_proof(&proof)?;
    let original_logs = logs(&source)?;
    let mut inputs = Vec::new();
    for (name, _) in FEATURES {
        let path = storage::safe_path(&source, Path::new(&format!("{name}.mp4")))?;
        inputs.push((path.clone(), digest(&path)?));
    }
    if output
        .parent()
        .ok_or("clip review parent absent")?
        .canonicalize()?
        .starts_with(&source)
    {
        return Err("clip review must be outside original captures".into());
    }
    let output = new_output(&source, output)?;
    let sheets = output.join("review-clips");
    fs::create_dir(&sheets)?;
    let mut runner = Runner::new(&output)?;
    let mut files = Vec::new();
    for ((name, title), (path, hash)) in FEATURES.iter().zip(&inputs) {
        let text = path.to_str().ok_or("clip path is not UTF-8")?;
        let probe = runner.command(
            "/usr/bin/ffprobe",
            &[
                "-v",
                "error",
                "-show_streams",
                "-show_format",
                "-of",
                "json",
                text,
            ]
            .map(String::from),
        )?;
        let duration = validate_media(&storage::parse_json(&probe)?)?;
        runner.command(
            "/usr/bin/ffmpeg",
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-threads",
                "1",
                "-i",
                text,
                "-xerror",
                "-f",
                "null",
                "-",
            ]
            .map(String::from),
        )?;
        let sheet = sheets.join(format!("{name}-%02d.png"));
        runner.command("/usr/bin/ffmpeg",&["-hide_banner","-loglevel","error","-threads","1","-i",text,"-vf","fps=2,scale=400:300,drawtext=text='%{pts\\:hms}':fontsize=18:fontcolor=white:box=1:boxcolor=black:x=5:y=5,tile=4x4","-threads","1",sheet.to_str().ok_or("sheet path is not UTF-8")?].map(String::from))?;
        let poster = sheets.join(format!("{name}-full.png"));
        runner.command(
            "/usr/bin/ffmpeg",
            &[
                "-hide_banner",
                "-loglevel",
                "error",
                "-threads",
                "1",
                "-ss",
                "2",
                "-i",
                text,
                "-frames:v",
                "1",
                "-threads",
                "1",
                poster.to_str().ok_or("poster path is not UTF-8")?,
            ]
            .map(String::from),
        )?;
        if fs::metadata(&poster)?.len() == 0 || !sheets.join(format!("{name}-01.png")).is_file() {
            return Err("decoded clip review images absent".into());
        }
        files.push(json!({"name":name,"title":title,"duration":duration,"sha256":hash,"bytes":fs::metadata(path)?.len()}));
    }
    if read(&source, "clips-result.json")? != record
        || read(&source, "restoration-proof.json")? != proof
        || logs(&source)? != original_logs
    {
        return Err("capture proof/log inputs changed during review".into());
    }
    for (path, hash) in inputs {
        if digest(&path)? != hash {
            return Err("capture media changed during review".into());
        }
    }
    let report = json!({"files":files,"full_decode":true,"audio_streams":0,"subtitle_streams":0,"visual_privacy_review":"pending; inspect all sheets and full-resolution scenes before README replacement"});
    write_report(&output, "clips-inspection.json", &report)?;
    Ok(report)
}
