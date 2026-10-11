use crate::{process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
    time::Duration,
};

pub const FRAMES: [u64; 29] = [
    0, 74, 75, 120, 155, 182, 197, 242, 308, 348, 349, 368, 388, 389, 403, 418, 419, 434, 436, 437,
    460, 471, 472, 481, 506, 531, 556, 557, 574,
];

pub fn validate_media(info: &Value) -> Result<f64> {
    let streams = info["streams"]
        .as_array()
        .ok_or("media stream inventory absent")?;
    if streams.len() != 1 {
        return Err("approved action must contain one video stream and no audio/subtitles".into());
    }
    let video = &streams[0];
    if video["codec_type"] != "video"
        || video["width"] != 1920
        || video["height"] != 1440
        || video["nb_frames"] != "575"
    {
        return Err("approved action geometry/frame count mismatch".into());
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

pub(crate) fn digest(path: &Path) -> Result<String> {
    if !path.symlink_metadata()?.file_type().is_file() {
        return Err("review input must be a regular file".into());
    }
    storage::sha256(File::open(path)?)
}
pub(crate) fn read(root: &Path, relative: &str) -> Result<Value> {
    let path = storage::safe_path(root, Path::new(relative))?;
    storage::parse_json(&fs::read(path)?)
}
pub fn validate_sources(root: &Path) -> Result<Value> {
    let manifest = read(root, "docs/media/features/manifest.json")?;
    let files = manifest["files"]
        .as_array()
        .ok_or("approved feature manifest absent")?;
    if files.is_empty() {
        return Err("approved feature manifest cannot be empty".into());
    }
    let mut names = BTreeSet::new();
    let mut hashes = BTreeMap::new();
    for entry in files {
        let name = entry["name"]
            .as_str()
            .ok_or("approved source name absent")?;
        storage::relative_path(Path::new(name))?;
        if Path::new(name).components().count() != 1 || !names.insert(name.to_string()) {
            return Err("unsafe or duplicate approved source name".into());
        }
        let path = storage::safe_path(
            root,
            Path::new(&format!("video/readme-promo/public/{name}.mp4")),
        )?;
        let actual = digest(&path)?;
        if entry["sha256"].as_str() != Some(actual.as_str()) {
            return Err("approved public source bytes changed".into());
        }
        hashes.insert(name.to_string(), actual);
    }
    let plugin = read(root, "video/readme-promo/out/plugins-on-source.json")?;
    let plugin_path =
        storage::safe_path(root, Path::new("video/readme-promo/public/plugins-on.mp4"))?;
    let plugin_digest = digest(&plugin_path)?;
    if plugin["sha256"].as_str() != Some(plugin_digest.as_str()) {
        return Err("isolated plugins-on source changed".into());
    }
    let events = plugin["events"]
        .as_array()
        .ok_or("plugin source events absent")?;
    if events.last().ok_or("plugin source events empty")?["enabled"]
        != json!(["clock", "system-status", "pane-pwd"])
    {
        return Err("approved plugin activation boundary mismatch".into());
    }
    let directories = plugin["observed_pane_directories"]
        .as_array()
        .ok_or("observed pane directories absent")?;
    let directories = directories
        .iter()
        .map(|value| {
            value
                .as_str()
                .ok_or("observed pane directory must be a string")
        })
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    if directories.len() != 3 {
        return Err("plugins-on source must observe three distinct pane directories".into());
    }
    Ok(json!({"approved_public_source_hashes":hashes,"plugins_on_sha256":plugin_digest}))
}

fn lower_priority() -> Result<()> {
    let mut allowed = unsafe { std::mem::zeroed::<nix::libc::cpu_set_t>() };
    if unsafe { nix::libc::sched_getaffinity(0, std::mem::size_of_val(&allowed), &mut allowed) }
        != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let first = (0..nix::libc::CPU_SETSIZE as usize)
        .find(|cpu| unsafe { nix::libc::CPU_ISSET(*cpu, &allowed) })
        .ok_or("no permitted review CPU")?;
    let mut selected = unsafe { std::mem::zeroed::<nix::libc::cpu_set_t>() };
    unsafe {
        nix::libc::CPU_SET(first, &mut selected);
    }
    if unsafe { nix::libc::sched_setaffinity(0, std::mem::size_of_val(&selected), &selected) } != 0
        || unsafe { nix::libc::setpriority(nix::libc::PRIO_PROCESS, 0, 19) } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

pub(crate) struct Runner<'a> {
    output: &'a Path,
    env: BTreeMap<OsString, OsString>,
    sequence: usize,
}
impl Runner<'_> {
    pub(crate) fn new(output: &Path) -> Result<Runner<'_>> {
        lower_priority()?;
        Ok(Runner {
            output,
            sequence: 0,
            env: BTreeMap::from([
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("LANG".into(), "C.UTF-8".into()),
                ("OMP_NUM_THREADS".into(), "1".into()),
                ("HOME".into(), output.as_os_str().into()),
                ("TMPDIR".into(), output.as_os_str().into()),
            ]),
        })
    }
    pub(crate) fn command(&mut self, program: &str, args: &[String]) -> Result<Vec<u8>> {
        self.command_timeout(program, args, Duration::from_secs(180))
    }
    pub(crate) fn command_timeout(
        &mut self,
        program: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        self.sequence += 1;
        let stdout = self
            .output
            .join(format!("command-{}.stdout", self.sequence));
        let stderr = self
            .output
            .join(format!("command-{}.stderr", self.sequence));
        let out = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&stdout)?;
        let err = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stderr)?;
        let arguments = args.iter().map(String::as_str).collect::<Vec<_>>();
        let mut process = OwnedProcess::spawn_split(
            Path::new(program),
            &arguments,
            &self.env,
            self.output,
            &out,
            &err,
        )?;
        if !process.wait(timeout)?.success() {
            return Err(format!(
                "encoded review command {} failed; retained evidence in {}",
                self.sequence,
                self.output.display()
            )
            .into());
        }
        Ok(fs::read(stdout)?)
    }
}

pub(crate) fn new_output(root: &Path, output: &Path) -> Result<std::path::PathBuf> {
    let parent = output.parent().ok_or("review output parent absent")?;
    if !parent.is_dir() || parent.symlink_metadata()?.file_type().is_symlink() {
        return Err("review output requires an existing real parent".into());
    }
    let parent = parent.canonicalize()?;
    for ancestor in root.ancestors() {
        let protected = ancestor.join("video/readme-promo");
        let protected = if protected.exists() {
            protected.canonicalize()?
        } else {
            protected
        };
        if parent.starts_with(&protected) {
            return Err("review evidence must be outside the approved montage workspace".into());
        }
    }
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(output)?;
    output.canonicalize().map_err(Into::into)
}

pub(crate) fn write_report(output: &Path, name: &str, report: &Value) -> Result<()> {
    storage::relative_path(Path::new(name))?;
    if Path::new(name).components().count() != 1 {
        return Err("unsafe review report name".into());
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output.join(name))?;
    file.write_all(&serde_json::to_vec_pretty(report)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

pub fn review(root: &Path, output: &Path) -> Result<Value> {
    let root = root.canonicalize()?;
    let video = storage::safe_path(&root, Path::new("video/readme-promo/out/action-v5.mp4"))?;
    let original = digest(&video)?;
    let sources = validate_sources(&root)?;
    let output = new_output(&root, output)?;
    let mut runner = Runner::new(&output)?;
    let info = runner.command(
        "/usr/bin/ffprobe",
        &[
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
            video.to_str().ok_or("video path is not UTF-8")?,
        ]
        .map(String::from),
    )?;
    let info = storage::parse_json(&info)?;
    let duration = validate_media(&info)?;
    runner.command(
        "/usr/bin/ffmpeg",
        &[
            "-v",
            "error",
            "-threads",
            "1",
            "-i",
            video.to_str().ok_or("video path is not UTF-8")?,
            "-xerror",
            "-f",
            "null",
            "-",
        ]
        .map(String::from),
    )?;
    let timeline = output.join("timeline-%02d.jpg");
    runner.command("/usr/bin/ffmpeg",&["-v","error","-threads","1","-i",video.to_str().ok_or("video path is not UTF-8")?,"-vf","fps=5,scale=480:360,drawtext=text='%{pts\\:hms}':fontcolor=white:fontsize=18:box=1:boxcolor=black@0.8,tile=5x5","-fps_mode","vfr","-threads","1",timeline.to_str().ok_or("timeline path is not UTF-8")?].map(String::from))?;
    for frame in FRAMES {
        let destination = output.join(format!("frame-{frame}.png"));
        let filter = format!("select=eq(n\\,{frame})");
        runner.command(
            "/usr/bin/ffmpeg",
            &[
                "-v",
                "error",
                "-threads",
                "1",
                "-i",
                video.to_str().ok_or("video path is not UTF-8")?,
                "-vf",
                &filter,
                "-frames:v",
                "1",
                "-threads",
                "1",
                destination.to_str().ok_or("frame path is not UTF-8")?,
            ]
            .map(String::from),
        )?;
        if fs::metadata(destination)?.len() == 0 {
            return Err("requested review frame was not decoded".into());
        }
    }
    if digest(&video)? != original || validate_sources(&root)? != sources {
        return Err("approved review inputs changed during decode".into());
    }
    let mut timelines = 0;
    for entry in fs::read_dir(&output)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().ok_or("review filename is not UTF-8")?;
        if name.starts_with("timeline-") && name.ends_with(".jpg") {
            if !entry.file_type()?.is_file() || entry.metadata()?.len() == 0 {
                return Err("review timeline image was not decoded".into());
            }
            timelines += 1;
        }
    }
    if timelines == 0 {
        return Err("no encoded timeline image was produced".into());
    }
    let report = json!({"sha256":original,"bytes":fs::metadata(&video)?.len(),"duration":duration,"frames":575,"full_decode":true,"audio_streams":0,"subtitle_streams":0,"approved_source_hashes_match":true,"isolated_plugins_on_source_hash_match":true});
    write_report(&output, "action-v5-manifest.json", &report)?;
    Ok(report)
}

#[cfg(test)]
mod output_tests {
    use super::*;

    #[test]
    fn nested_capture_tools_cannot_write_into_the_approved_montage_workspace() -> Result<()> {
        let root = storage::private_directory(&std::env::temp_dir(), "among-media-output")?;
        let capture = root.join("private/capture");
        let protected = root.join("video/readme-promo/out");
        fs::create_dir_all(&capture)?;
        fs::create_dir_all(&protected)?;
        let output = protected.join("must-not-exist");
        assert!(new_output(&capture, &output).is_err());
        assert!(!output.exists());
        let allowed = root.join("private/new-evidence");
        assert_eq!(new_output(&capture, &allowed)?, allowed);
        fs::remove_dir_all(root)?;
        Ok(())
    }
}
