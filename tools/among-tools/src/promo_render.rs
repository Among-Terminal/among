use crate::{
    action_review::{digest, new_output, read, write_report, Runner},
    promo_review, storage, Result,
};
use serde_json::{json, Value};
use std::{fs, path::Path, time::Duration};

pub fn render_arguments(entry: &Path, public: &Path, destination: &Path) -> Result<Vec<String>> {
    let text = |path: &Path| -> Result<String> {
        path.to_str()
            .map(String::from)
            .ok_or_else(|| "render path is not UTF-8".into())
    };
    Ok(vec![
        "render".into(),
        text(entry)?,
        "AmongPromo".into(),
        text(destination)?,
        "--codec=h264".into(),
        "--crf=20".into(),
        "--concurrency=1".into(),
        "--muted".into(),
        "--timeout=180000".into(),
        "--offthreadvideo-cache-size-in-bytes=134217728".into(),
        "--log=error".into(),
        format!("--public-dir={}", text(public)?),
    ])
}

pub fn render(root: &Path, output: &Path) -> Result<Value> {
    render_kind(root, output, false)
}

pub fn render_action(root: &Path, output: &Path) -> Result<Value> {
    render_kind(root, output, true)
}

fn render_kind(root: &Path, output: &Path, action: bool) -> Result<Value> {
    let root = root.canonicalize()?;
    let action_sources = if action {
        Some(crate::action_review::validate_sources(&root)?)
    } else {
        None
    };
    let project = root.join("video/readme-promo");
    let remotion = project.join("node_modules/.bin/remotion").canonicalize()?;
    let dependencies = project.join("node_modules").canonicalize()?;
    if !remotion.starts_with(&dependencies) || !remotion.is_file() {
        return Err("renderer must resolve within the installed project dependencies".into());
    }
    let browser = dependencies.join(".remotion/chrome-headless-shell/linux64/chrome-headless-shell-linux64/chrome-headless-shell").canonicalize()?;
    if !browser.starts_with(&dependencies) || !browser.is_file() {
        return Err("renderer requires a preinstalled project-local browser".into());
    }
    let inputs = [
        "src/index.ts",
        "src/Promo.tsx",
        "src/Action.tsx",
        "tsconfig.json",
        "package.json",
        "package-lock.json",
    ];
    let mut originals = Vec::new();
    for relative in inputs {
        let path = storage::safe_path(&root, Path::new(&format!("video/readme-promo/{relative}")))?;
        originals.push((relative, digest(&path)?, path));
    }
    let timeline = read(&root, "video/readme-promo/src/timeline.json")?;
    let source_identity = promo_review::validate_sources(&root, &timeline)?;
    let mut frozen = vec![
        "src/timeline.json".to_string(),
        "public/hero.png".into(),
        "public/end.png".into(),
        "public/brand.ttf".into(),
    ];
    for shot in timeline["shots"]
        .as_array()
        .ok_or("prepared shots absent")?
    {
        let name = shot["src"].as_str().ok_or("prepared filename absent")?;
        frozen.push(format!("public/{name}"));
        let path = storage::safe_path(
            &root,
            Path::new(&format!("video/readme-promo/public/{name}")),
        )?;
        if shot["sha256"].as_str() != Some(digest(&path)?.as_str()) {
            return Err("frozen clip is not the reviewed source".into());
        }
    }
    let hero = storage::safe_path(&root, Path::new("video/readme-promo/public/hero.png"))?;
    let reviewed_hero =
        storage::safe_path(&root, Path::new("docs/media/features/02-split-resize.png"))?;
    if digest(&hero)? != digest(&reviewed_hero)? {
        return Err("frozen hero differs from reviewed poster".into());
    }
    let output = new_output(&root, output)?;
    fs::create_dir(output.join("src"))?;
    fs::create_dir(output.join("public"))?;
    let mut frozen_originals = Vec::new();
    for relative in frozen {
        let path = storage::safe_path(&root, Path::new(&format!("video/readme-promo/{relative}")))?;
        let hash = digest(&path)?;
        fs::copy(&path, output.join(&relative))?;
        if digest(&output.join(&relative))? != hash {
            return Err("frozen render input changed during private copy".into());
        }
        frozen_originals.push((relative, hash, path));
    }
    let assets = json!({"timeline":digest(&output.join("src/timeline.json"))?, "hero":digest(&output.join("public/hero.png"))?, "end":digest(&output.join("public/end.png"))?, "brand":digest(&output.join("public/brand.ttf"))?});
    write_report(&output, "prepared-assets.json", &assets)?;
    let prepared = json!({"duration":timeline["total"].as_u64().ok_or("prepared total absent")? as f64 / timeline["fps"].as_u64().ok_or("prepared fps absent")? as f64,"source_count":13,"total":timeline["total"],"origin":"existing frozen assets; no preparation or font resolution"});
    let plugin = if action {
        let path =
            storage::safe_path(&root, Path::new("video/readme-promo/public/plugins-on.mp4"))?;
        let hash = digest(&path)?;
        let destination = output.join("public/plugins-on.mp4");
        fs::copy(&path, &destination)?;
        if digest(&destination)? != hash {
            return Err("plugin capture changed during private copy".into());
        }
        Some((path, hash, destination))
    } else {
        None
    };
    for (relative, hash, path) in &originals {
        fs::copy(path, output.join(relative))?;
        if digest(&output.join(relative))? != *hash {
            return Err("composition changed during private copy".into());
        }
    }
    // Webpack writes under node_modules/.cache; only the dependency entries are
    // shared read-only, so its cache stays in the disposable workspace.
    let private_dependencies = output.join("node_modules");
    fs::create_dir(&private_dependencies)?;
    for entry in fs::read_dir(&dependencies)? {
        let entry = entry?;
        if entry.file_name() != ".cache" {
            std::os::unix::fs::symlink(entry.path(), private_dependencies.join(entry.file_name()))?;
        }
    }
    let logs = storage::private_directory(&output, "render-logs")?;
    let mut runner = Runner::new(&logs)?;
    let rendered = output.join(if action {
        "action-v5-render.mp4"
    } else {
        "among-terminal-promo-render.mp4"
    });
    let delivered = output.join(if action {
        "action-v5.mp4"
    } else {
        "among-terminal-promo.mp4"
    });
    let mut args = render_arguments(
        &output.join("src/index.ts"),
        &output.join("public"),
        &rendered,
    )?;
    if action {
        args[2] = "AmongActionPromo".into();
    }
    args.push(format!(
        "--browser-executable={}",
        browser.to_str().ok_or("browser path is not UTF-8")?
    ));
    runner.command_timeout(
        remotion.to_str().ok_or("renderer path is not UTF-8")?,
        &args,
        Duration::from_secs(1800),
    )?;
    runner.command(
        "/usr/bin/ffmpeg",
        &[
            "-v",
            "error",
            "-i",
            rendered.to_str().ok_or("render path is not UTF-8")?,
            "-map",
            "0:v:0",
            "-c:v",
            "copy",
            "-an",
            "-map_metadata",
            "-1",
            "-movflags",
            "+faststart",
            delivered.to_str().ok_or("delivery path is not UTF-8")?,
        ]
        .map(String::from),
    )?;
    let info = runner.command(
        "/usr/bin/ffprobe",
        &[
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
            delivered.to_str().ok_or("delivery path is not UTF-8")?,
        ]
        .map(String::from),
    )?;
    let info = storage::parse_json(&info)?;
    let duration = if action {
        crate::action_review::validate_media(&info)?
    } else {
        promo_review::validate_media(&info, &timeline)?
    };
    runner.command(
        "/usr/bin/ffmpeg",
        &[
            "-v",
            "error",
            "-threads",
            "1",
            "-i",
            delivered.to_str().ok_or("delivery path is not UTF-8")?,
            "-xerror",
            "-f",
            "null",
            "-",
        ]
        .map(String::from),
    )?;
    for (relative, hash, path) in &originals {
        if digest(path)? != *hash || digest(&output.join(relative))? != *hash {
            return Err("original composition changed during render".into());
        }
    }
    for (relative, hash, path) in frozen_originals {
        if digest(&path)? != hash || digest(&output.join(relative))? != hash {
            return Err("frozen render inputs changed during render".into());
        }
    }
    if read(&root, "video/readme-promo/src/timeline.json")? != timeline
        || promo_review::validate_sources(&root, &timeline)? != source_identity
    {
        return Err("render source association changed".into());
    }
    if read(&output, "prepared-assets.json")? != assets
        || read(&output, "src/timeline.json")? != timeline
    {
        return Err("prepared manifests changed during render".into());
    }
    for (key, relative) in [
        ("timeline", "src/timeline.json"),
        ("hero", "public/hero.png"),
        ("end", "public/end.png"),
        ("brand", "public/brand.ttf"),
    ] {
        if assets[key].as_str() != Some(digest(&output.join(relative))?.as_str()) {
            return Err("prepared assets changed during render".into());
        }
    }
    for shot in timeline["shots"]
        .as_array()
        .ok_or("prepared shots absent")?
    {
        let name = shot["src"]
            .as_str()
            .ok_or("prepared clip filename absent")?;
        if shot["sha256"].as_str()
            != Some(
                digest(&storage::safe_path(
                    &output,
                    Path::new(&format!("public/{name}")),
                )?)?
                .as_str(),
            )
        {
            return Err("prepared clip changed during render".into());
        }
    }
    if let Some((path, hash, destination)) = plugin {
        if digest(&path)? != hash
            || digest(&destination)? != hash
            || Some(crate::action_review::validate_sources(&root)?) != action_sources
        {
            return Err("approved plugin capture inputs changed during render".into());
        }
    }
    let report = json!({"composition": if action { "AmongActionPromo" } else { "AmongPromo" }, "preparation": prepared, "duration": duration, "sha256": digest(&delivered)?, "bytes": fs::metadata(&delivered)?.len(), "full_decode": true, "audio_streams": 0, "subtitle_streams": 0, "visual_review": "pending", "approved_delivery_replaced": false});
    write_report(&output, "render-manifest.json", &report)?;
    Ok(report)
}
