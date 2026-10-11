use crate::{
    action_review::{digest, new_output, write_report, Runner},
    storage, Result,
};
use serde_json::{json, Value};
use std::{fs, path::Path};

pub fn events(value: &Value) -> Result<Vec<(f64, f64, String)>> {
    let values = value
        .as_array()
        .filter(|v| v.len() <= 1000)
        .ok_or("recorded shortcut events must be a bounded array")?;
    let mut result = Vec::new();
    for (index, event) in values.iter().enumerate() {
        let begin = event["start"]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0 && *v <= 3600.0)
            .ok_or("invalid recorded shortcut start")?;
        let next = if index + 1 < values.len() {
            values[index + 1]["start"]
                .as_f64()
                .filter(|v| v.is_finite())
                .ok_or("invalid next shortcut start")?
        } else {
            begin + 1.6
        };
        let end = (begin + 1.6).min(next);
        let text = event["keys"]
            .as_str()
            .ok_or("recorded shortcut text absent")?;
        if end <= begin
            || text.is_empty()
            || text.len() > 32
            || !text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b',' | b' '))
        {
            return Err("invalid recorded shortcut event".into());
        }
        result.push((begin, end, text.to_string()));
    }
    Ok(result)
}

pub fn badge_arguments(
    width: u64,
    height: u64,
    text: &str,
    destination: &str,
) -> Result<Vec<String>> {
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return Err("invalid measured shortcut label dimensions".into());
    }
    let width = width + 60;
    let height = height + 36;
    Ok(vec![
        "-size".into(),
        format!("{width}x{height}"),
        "xc:none".into(),
        "-fill".into(),
        "black".into(),
        "-draw".into(),
        format!("roundrectangle 0,0,{},{},18,18", width - 1, height - 1),
        "-font".into(),
        "DejaVu-Sans-Bold".into(),
        "-pointsize".into(),
        "32".into(),
        "-fill".into(),
        "white".into(),
        "-gravity".into(),
        "center".into(),
        "-annotate".into(),
        "+0+0".into(),
        text.into(),
        destination.into(),
    ])
}

pub fn graph(events: &[(f64, f64, String)]) -> Vec<String> {
    if events.is_empty() {
        return vec!["-vf".into(), "null".into()];
    }
    let filters=events.iter().enumerate().map(|(index,(begin,end,_))| {
        let source=if index==0 {"0:v".to_string()} else {format!("badge{index}")};
        format!("[{source}][{}:v]overlay=x=(W-w)/2:y=(H-h)/2:enable='between(t,{begin},{end})':repeatlast=1[badge{}]",index+1,index+1)
    }).collect::<Vec<_>>().join(";");
    vec![
        "-filter_complex".into(),
        filters,
        "-map".into(),
        format!("[badge{}]", events.len()),
    ]
}

pub fn encode(raw: &Path, event_file: &Path, output: &Path) -> Result<Value> {
    if raw.symlink_metadata()?.file_type().is_symlink()
        || event_file.symlink_metadata()?.file_type().is_symlink()
    {
        return Err("raw clip/events must not be symlinks".into());
    }
    let raw = raw.canonicalize()?;
    let parent = raw.parent().ok_or("raw clip parent absent")?;
    let raw = storage::safe_path(
        parent,
        Path::new(raw.file_name().ok_or("raw clip basename absent")?),
    )?;
    let event_file = event_file.canonicalize()?;
    let bytes = fs::read(&event_file)?;
    let parsed = events(&storage::parse_json(&bytes)?)?;
    let original = digest(&raw)?;
    let output = new_output(parent, output)?;
    let mut runner = Runner::new(&output)?;
    let mut args = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-threads".into(),
        "1".into(),
        "-i".into(),
        raw.to_str().ok_or("raw clip path is not UTF-8")?.into(),
    ];
    if !parsed.is_empty() {
        fs::create_dir(output.join("key-badges"))?;
    }
    for (index, (_, _, text)) in parsed.iter().enumerate() {
        let measured = runner.command(
            "/usr/bin/convert",
            &[
                "-font",
                "DejaVu-Sans-Bold",
                "-pointsize",
                "32",
                &format!("label:{text}"),
                "-format",
                "%w %h",
                "info:",
            ]
            .map(String::from),
        )?;
        let measured = String::from_utf8(measured)?
            .split_whitespace()
            .map(str::parse::<u64>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let [width, height]: [u64; 2] = measured
            .try_into()
            .map_err(|_| "unexpected shortcut label measurement")?;
        let badge = format!("key-badges/badge-{index}.png");
        runner.command(
            "/usr/bin/convert",
            &badge_arguments(width, height, text, &badge)?,
        )?;
        if fs::metadata(output.join(&badge))?.len() == 0 {
            return Err("shortcut badge not rendered".into());
        }
        args.extend(["-i".into(), badge]);
    }
    args.extend(graph(&parsed));
    args.extend(
        [
            "-an",
            "-map_metadata",
            "-1",
            "-c:v",
            "libx264",
            "-preset",
            "fast",
            "-crf",
            "20",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
            "clip.mp4",
        ]
        .map(String::from),
    );
    runner.command("/usr/bin/ffmpeg", &args)?;
    if digest(&raw)? != original || fs::read(&event_file)? != bytes {
        return Err("recorded raw clip/events changed during encode".into());
    }
    let video = output.join("clip.mp4");
    if fs::metadata(&video)?.len() == 0 {
        return Err("encoded feature clip absent".into());
    }
    let report = json!({"source_sha256":original,"events_sha256":storage::sha256(bytes.as_slice())?,"sha256":digest(&video)?,"bytes":fs::metadata(video)?.len(),"recorded_events":parsed.len(),"review":"pending media decode and visual/privacy review"});
    write_report(&output, "clip-encoding.json", &report)?;
    Ok(report)
}
