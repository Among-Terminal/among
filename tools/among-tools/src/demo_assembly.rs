use crate::{
    action_review::{digest, new_output, read, write_report, Runner},
    storage, Result,
};
use serde_json::{json, Value};
use std::{collections::BTreeSet, fs, path::Path, time::Duration};

pub fn plan(manifest: &Value) -> Result<(String, Value, f64, f64)> {
    let entries = manifest["files"]
        .as_array()
        .filter(|v| v.len() >= 2 && v.len() <= 100)
        .ok_or("assembly requires at least two bounded reviewed clips")?;
    let mut names = BTreeSet::new();
    let mut elapsed = 0.0;
    let mut filters = Vec::new();
    let mut chapters = Vec::new();
    for (index, item) in entries.iter().enumerate() {
        let name = item["name"].as_str().ok_or("clip name absent")?;
        storage::relative_path(Path::new(name))?;
        if Path::new(name).components().count() != 1 || !names.insert(name) {
            return Err("unsafe or duplicate clip name".into());
        }
        let title = item["title"].as_str().ok_or("clip title absent")?;
        let hash = item["sha256"].as_str().ok_or("clip hash absent")?;
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
        {
            return Err("invalid clip digest".into());
        }
        let duration = item["duration"]
            .as_f64()
            .filter(|v| v.is_finite() && *v > 0.0 && *v <= 3600.0)
            .ok_or("invalid clip duration")?;
        filters.push(format!("[{index}:v]setpts=PTS-STARTPTS,setsar=1[v{index}]"));
        let start = format!("{elapsed:.2}").parse::<f64>()?;
        chapters.push(json!({"name":name,"title":title,"start":start,"duration":duration,"source_sha256":hash}));
        elapsed += duration;
    }
    filters.push(format!(
        "{}concat=n={}:v=1:a=0[out]",
        (0..entries.len())
            .map(|i| format!("[v{i}]"))
            .collect::<String>(),
        entries.len()
    ));
    let poster = chapters[1]["start"]
        .as_f64()
        .ok_or("second chapter timestamp absent")?
        + 6.0;
    Ok((
        filters.join(";"),
        json!(chapters),
        format!("{elapsed:.2}").parse()?,
        poster,
    ))
}

pub fn assemble(source: &Path, output: &Path) -> Result<Value> {
    let source = source.canonicalize()?;
    let manifest = read(&source, "manifest.json")?;
    let (graph, chapters, duration, poster_second) = plan(&manifest)?;
    let mut inputs = Vec::new();
    for item in manifest["files"].as_array().ok_or("clip manifest absent")? {
        let name = item["name"].as_str().ok_or("clip name absent")?;
        let path = storage::safe_path(&source, Path::new(&format!("{name}.mp4")))?;
        let hash = digest(&path)?;
        if item["sha256"].as_str() != Some(hash.as_str()) {
            return Err("unreviewed assembly input".into());
        }
        inputs.push((path, hash));
    }
    if output
        .parent()
        .ok_or("assembly output parent absent")?
        .canonicalize()?
        .starts_with(&source)
    {
        return Err("assembly output must be outside original reviewed inputs".into());
    }
    let output = new_output(&source, output)?;
    let mut runner = Runner::new(&output)?;
    let mut args = vec!["-hide_banner".into(), "-loglevel".into(), "error".into()];
    for (path, _) in &inputs {
        args.push("-i".into());
        args.push(path.to_str().ok_or("clip path is not UTF-8")?.into());
    }
    args.extend(
        [
            "-filter_complex_threads",
            "1",
            "-filter_complex",
            &graph,
            "-map",
            "[out]",
            "-an",
            "-map_metadata",
            "-1",
            "-c:v",
            "libx264",
            "-preset",
            "slow",
            "-crf",
            "20",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
            "among-terminal-overview.mp4",
        ]
        .map(String::from),
    );
    runner.command_timeout("/usr/bin/ffmpeg", &args, Duration::from_secs(1800))?;
    runner.command(
        "/usr/bin/ffmpeg",
        &[
            "-hide_banner",
            "-loglevel",
            "error",
            "-ss",
            &poster_second.to_string(),
            "-i",
            "among-terminal-overview.mp4",
            "-frames:v",
            "1",
            "-threads",
            "1",
            "among-terminal-overview.png",
        ]
        .map(String::from),
    )?;
    let video = output.join("among-terminal-overview.mp4");
    let poster = output.join("among-terminal-overview.png");
    if fs::metadata(&video)?.len() == 0 || fs::metadata(&poster)?.len() == 0 {
        return Err("assembly media/poster absent".into());
    }
    if read(&source, "manifest.json")? != manifest {
        return Err("assembly manifest changed during encoding".into());
    }
    for (path, hash) in inputs {
        if digest(&path)? != hash {
            return Err("reviewed clip changed during assembly".into());
        }
    }
    let report = json!({"chapters":chapters,"duration":duration,"sha256":digest(&video)?,"bytes":fs::metadata(video)?.len(),"poster_sha256":digest(&poster)?,"poster_second":poster_second,"review":"pending decode, visual and browser checks"});
    write_report(&output, "overview-manifest.json", &report)?;
    Ok(report)
}
