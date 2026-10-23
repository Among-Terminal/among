//! artifact-bound external probes; installed-launcher and extracted-loader
//! scopes are reported separately and neither stands in for General controls.
use crate::{
    audio_capture::{self, utf8, Invocation},
    deb,
    process::OwnedProcess,
    storage, Result,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs::{self, File},
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: u32,
    pub source_sha: String,
    pub architecture: String,
    pub artifact_sha256: String,
    pub probe_sha256: String,
}

fn source(source: &str) -> Result<()> {
    if source.len() != 40
        || !source
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    {
        return Err("invalid audio source commit".into());
    }
    Ok(())
}
fn digest(path: &Path) -> Result<String> {
    storage::sha256(File::open(path)?)
}

pub fn bind(
    probe: &Path,
    artifact: &Path,
    commit: &str,
    architecture: &str,
    output: &Path,
) -> Result<()> {
    source(commit)?;
    deb::elf_architecture(probe, architecture)?;
    if !probe.symlink_metadata()?.is_file() || !artifact.symlink_metadata()?.is_file() {
        return Err("audio inputs must be regular files".into());
    }
    fs::create_dir(output)?;
    fs::copy(probe, output.join("audio-probe"))?;
    fs::set_permissions(
        output.join("audio-probe"),
        fs::Permissions::from_mode(0o755),
    )?;
    let receipt = Receipt {
        schema: 1,
        source_sha: commit.into(),
        architecture: architecture.into(),
        artifact_sha256: digest(artifact)?,
        probe_sha256: digest(probe)?,
    };
    fs::write(
        output.join("audio-receipt.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;
    Ok(())
}

pub fn validate(
    directory: &Path,
    artifact: &Path,
    commit: &str,
    architecture: &str,
) -> Result<Receipt> {
    source(commit)?;
    let receipt: Receipt = serde_json::from_value(storage::parse_json(&fs::read(
        directory.join("audio-receipt.json"),
    )?)?)?;
    let probe = storage::safe_path(directory, Path::new("audio-probe"))?;
    if receipt.schema != 1
        || receipt.source_sha != commit
        || receipt.architecture != architecture
        || receipt.artifact_sha256 != digest(artifact)?
        || receipt.probe_sha256 != digest(&probe)?
    {
        return Err("audio probe source/artifact/architecture/digest association differs".into());
    }
    deb::elf_architecture(&probe, architecture)?;
    Ok(receipt)
}

fn command(program: &str, args: &[&str], cwd: &Path, label: &str) -> Result<String> {
    let log = cwd.join(format!("{label}.log"));
    let env = BTreeMap::from([
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("LC_ALL".into(), "C.UTF-8".into()),
    ]);
    let mut child = OwnedProcess::spawn(Path::new(program), args, &env, cwd, &File::create(&log)?)?;
    if !child.wait(Duration::from_secs(120))?.success() {
        return Err(format!("audio package prerequisite failed: {label}").into());
    }
    Ok(fs::read_to_string(log)?.trim().to_string())
}

fn clone_tree(source: &Path, destination: &Path) -> Result<()> {
    let metadata = source.symlink_metadata()?;
    if metadata.is_dir() {
        fs::create_dir(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            clone_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else if metadata.file_type().is_symlink() {
        symlink(fs::read_link(source)?, destination)?;
    } else if metadata.is_file() {
        fs::hard_link(source, destination)?;
    } else {
        return Err("unexpected package member type in audio fixture".into());
    }
    Ok(())
}

fn manifest(root: &Path, directory: &Path, entries: &mut BTreeMap<String, String>) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let name = utf8(path.strip_prefix(root)?)?.to_string();
        let metadata = path.symlink_metadata()?;
        if metadata.is_dir() {
            manifest(root, &path, entries)?;
        } else if metadata.file_type().is_symlink() {
            entries.insert(name, format!("link:{}", fs::read_link(path)?.display()));
        } else if metadata.is_file() {
            entries.insert(name, digest(&path)?);
        } else {
            return Err("unexpected native-library member type".into());
        }
    }
    Ok(())
}

fn maps(root: &Path, evidence: &Path) -> Result<()> {
    let decoder = evidence.join("consumer/decoder-maps.txt");
    let text = fs::read_to_string(if decoder.is_file() {
        decoder
    } else {
        evidence.join("consumer/maps.txt")
    })?;
    for name in [
        "libgtk-4.so",
        "libgstreamer-1.0.so",
        "libmedia-gstreamer.so",
        "libgstwavparse.so",
        "libgstpulseaudio.so",
    ] {
        let paths: Vec<_> = text
            .lines()
            .filter_map(|line| line.split_whitespace().nth(5))
            .filter(|path| path.contains(name))
            .collect();
        if paths.is_empty() || paths.iter().any(|path| !Path::new(path).starts_with(root)) {
            return Err(format!("audio maps lack exclusively packaged {name}").into());
        }
    }
    for path in text
        .lines()
        .filter_map(|line| line.split_whitespace().nth(5))
    {
        if ["/libgst", "/gstreamer-1.0/", "/media/libmedia", "/libpulse"]
            .iter()
            .any(|name| path.contains(name))
            && !Path::new(path).starts_with(root)
        {
            return Err(
                format!("native audio library borrowed outside actual payload: {path}").into(),
            );
        }
    }
    let child = fs::read_to_string(evidence.join("consumer/host-child-maps.txt"))?;
    if child.contains(utf8(root)?) || child.contains("/snap/core24/") {
        return Err("audio payload leaked into host child".into());
    }
    Ok(())
}

pub fn run(
    kind: &str,
    artifact: &Path,
    checkers: &Path,
    commit: &str,
    architecture: &str,
    output: &Path,
) -> Result<()> {
    audio_capture::require_container()?;
    if !matches!(kind, "appimage" | "snap-core24" | "snap-installed") {
        return Err("unsupported audio package test scope".into());
    }
    let receipt = validate(checkers, artifact, commit, architecture)?;
    fs::create_dir(output)?;
    let base = output.canonicalize()?;
    let artifact = artifact.canonicalize()?;
    let checkers = checkers.canonicalize()?;
    let kernel = command("/usr/bin/uname", &["-m"], &base, "kernel-architecture")?;
    let native = match (std::env::consts::ARCH,kernel.as_str(),architecture) {
        ("x86_64","x86_64","amd64") | ("aarch64","aarch64","arm64") => true,
        _ => return Err("compiled, kernel and declared audio architecture differ; emulation is not native acceptance".into()),
    };
    fs::write(
        base.join("identity.json"),
        serde_json::to_vec_pretty(
            &json!({"receipt":receipt,"kernel":kernel,"compiled_arch":std::env::consts::ARCH,"architecture_agrees":native,
        "scope":kind,"general_controls":false,"runner_arch":std::env::var("RUNNER_ARCH").ok(),"github_run_id":std::env::var("GITHUB_RUN_ID").ok()}),
        )?,
    )?;
    let triplet = if architecture == "amd64" {
        "x86_64-linux-gnu"
    } else {
        "aarch64-linux-gnu"
    };
    // refuse borrowing instead of hiding installed native trees on a real host.
    for directory in [
        format!("/usr/lib/{triplet}/gtk-4.0/4.0.0/media"),
        format!("/usr/lib/{triplet}/gstreamer-1.0"),
    ] {
        if Path::new(&directory).is_dir()
            && fs::read_dir(&directory)?.any(|entry| {
                entry.is_ok_and(|entry| entry.path().extension().is_some_and(|s| s == "so"))
            })
        {
            return Err(
                "native media/plugin tree is visible; use an owned runtime without native backends"
                    .into(),
            );
        }
    }
    let payload = if kind == "appimage" {
        let image = base.join("candidate.AppImage");
        fs::copy(&artifact, &image)?;
        fs::set_permissions(&image, fs::Permissions::from_mode(0o755))?;
        command(utf8(&image)?, &["--appimage-extract"], &base, "extract")?;
        base.join("squashfs-root")
    } else {
        let extracted = base.join("snap-payload");
        command(
            "/usr/bin/unsquashfs",
            &["-no-progress", "-d", utf8(&extracted)?, utf8(&artifact)?],
            &base,
            "extract",
        )?;
        let metadata = crate::snap_runtime::snap_metadata(&artifact)?;
        if metadata["base"] != "core24"
            || metadata["confinement"] != "classic"
            || metadata["architectures"] != json!([architecture])
        {
            return Err(
                "actual Snap metadata differs from classic/core24/architecture contract".into(),
            );
        }
        fs::write(
            base.join("snap-metadata.json"),
            serde_json::to_vec_pretty(&metadata)?,
        )?;
        if kind == "snap-installed" {
            let installed = Path::new("/snap/among/current").canonicalize()?;
            if digest(&installed.join("bin/among"))? != digest(&extracted.join("bin/among"))? {
                return Err("installed Among differs from captured Snap artifact".into());
            }
            let (mut expected, mut observed) = (BTreeMap::new(), BTreeMap::new());
            manifest(
                &extracted.join("usr/lib"),
                &extracted.join("usr/lib"),
                &mut expected,
            )?;
            manifest(
                &installed.join("usr/lib"),
                &installed.join("usr/lib"),
                &mut observed,
            )?;
            if expected != observed {
                return Err(
                    "installed native library/module/helper bytes differ from exact Snap artifact"
                        .into(),
                );
            }
            fs::write(
                base.join("installed-native-files.json"),
                serde_json::to_vec_pretty(&observed)?,
            )?;
            installed
        } else {
            extracted
        }
    }
    .canonicalize()?;
    let lib = if kind == "appimage" {
        payload.join("usr/lib")
    } else {
        payload.join(format!("usr/lib/{triplet}"))
    };
    crate::audio_payload::require(&lib)?;
    let mut native_files = BTreeMap::new();
    manifest(&lib, &lib, &mut native_files)?;
    fs::write(
        base.join("native-files.json"),
        serde_json::to_vec_pretty(&native_files)?,
    )?;
    let mut notices = BTreeMap::new();
    for relative in ["usr/share/doc", "usr/share/common-licenses"] {
        let path = payload.join(relative);
        if path.is_dir() {
            manifest(&payload, &path, &mut notices)?;
        }
    }
    if notices.is_empty() {
        return Err("actual audio payload has no shipped native notice inventory".into());
    }
    fs::write(
        base.join("shipped-notices.json"),
        serde_json::to_vec_pretty(&notices)?,
    )?;
    let mut inventory = Vec::new();
    for member in crate::audio_payload::MEMBERS {
        inventory.push(json!({"path":member,"sha256":digest(&lib.join(member))?}));
    }
    fs::write(
        base.join("audio-members.json"),
        serde_json::to_vec_pretty(&inventory)?,
    )?;
    let probe = base.join("probe");
    fs::copy(checkers.join("audio-probe"), &probe)?;
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o755))?;
    let application = payload.join(if kind == "appimage" {
        "usr/bin/among"
    } else {
        "bin/among"
    });
    let original_runpath = command(
        "/usr/bin/patchelf",
        &["--print-rpath", utf8(&application)?],
        &base,
        "package-runpath",
    )?;
    if original_runpath.is_empty() {
        return Err("actual packaged application has no private runtime search path".into());
    }
    let origin = utf8(
        application
            .parent()
            .ok_or("packaged application parent absent")?,
    )?;
    let runpath = original_runpath
        .replace("${ORIGIN}", origin)
        .replace("$ORIGIN", origin);
    if kind != "appimage" {
        let interpreter = command(
            "/usr/bin/patchelf",
            &["--print-interpreter", utf8(&payload.join("bin/among"))?],
            &base,
            "package-interpreter",
        )?;
        if !interpreter.starts_with("/snap/core24/") || !Path::new(&interpreter).is_file() {
            return Err("Snap's actual core24 interpreter is unavailable".into());
        }
        command(
            "/usr/bin/patchelf",
            &["--set-interpreter", &interpreter, utf8(&probe)?],
            &base,
            "probe-interpreter",
        )?;
        fs::write(
            base.join("core24-metadata.yaml"),
            fs::read("/snap/core24/current/meta/snap.yaml")?,
        )?;
    }
    command(
        "/usr/bin/patchelf",
        &["--set-rpath", &runpath, utf8(&probe)?],
        &base,
        "probe-runpath",
    )?;
    fs::write(base.join("probe-runtime-sha256.txt"), digest(&probe)?)?;
    let fixtures = audio_capture::fixtures(&base.join("fixtures"))?;
    let mut results = Vec::new();
    for fixture in &fixtures {
        let label = fixture
            .file_stem()
            .ok_or("fixture name missing")?
            .to_str()
            .ok_or("fixture encoding")?;
        let evidence = base.join(format!("preview-{label}"));
        let analysis = audio_capture::run(
            &Invocation {
                probe: &probe,
                scenario: "preview-off",
                input: fixture,
                payload: &payload,
                installed: kind == "snap-installed",
                expected_failure: None,
                signal: true,
            },
            &evidence,
        )?;
        maps(&payload, &evidence)?;
        if kind == "snap-installed" {
            let context =
                storage::parse_json(&fs::read(evidence.join("consumer/process-context.json"))?)?;
            if context["snap"]
                .as_str()
                .and_then(|value| Path::new(value).canonicalize().ok())
                .as_ref()
                != Some(&payload)
                || context["snap_revision"].as_str()
                    != payload.file_name().and_then(|name| name.to_str())
                || context["confinement"].as_str().is_none_or(str::is_empty)
            {
                return Err(
                    "external probe did not enter the expected installed classic Snap context"
                        .into(),
                );
            }
        }
        if kind != "appimage"
            && !fs::read_to_string(evidence.join("consumer/decoder-maps.txt"))
                .or_else(|_| fs::read_to_string(evidence.join("consumer/maps.txt")))?
                .contains("/snap/core24/")
        {
            return Err("probe did not map the actual core24 loader/runtime".into());
        }
        results.push(json!({"case":label,"analysis":analysis}));
    }
    let silent = base.join("disabled");
    audio_capture::run(
        &Invocation {
            probe: &probe,
            scenario: "event-disabled",
            input: &fixtures[1],
            payload: &payload,
            installed: kind == "snap-installed",
            expected_failure: None,
            signal: false,
        },
        &silent,
    )?;
    for member in [
        crate::audio_payload::MEMBERS[0],
        "gstreamer-1.0/libgstwavparse.so",
        crate::audio_payload::MEMBERS[11],
    ] {
        let name = Path::new(member)
            .file_name()
            .ok_or("member name missing")?
            .to_str()
            .ok_or("member encoding")?;
        let broken = base.join(format!("missing-{name}-payload"));
        clone_tree(&payload, &broken)?;
        let relative = lib.strip_prefix(&payload)?;
        fs::remove_file(broken.join(relative).join(member))?;
        let negative_probe = base.join(format!("probe-missing-{name}"));
        fs::copy(&probe, &negative_probe)?;
        command(
            "/usr/bin/patchelf",
            &[
                "--set-rpath",
                &runpath.replace(utf8(&payload)?, utf8(&broken)?),
                utf8(&negative_probe)?,
            ],
            &base,
            &format!("negative-runpath-{name}"),
        )?;
        audio_capture::run(
            &Invocation {
                probe: &negative_probe,
                scenario: "preview-off",
                input: &fixtures[1],
                payload: &broken,
                installed: false,
                expected_failure: Some(name),
                signal: false,
            },
            &base.join(format!("missing-{name}")),
        )?;
    }
    if digest(&artifact)? != receipt.artifact_sha256 {
        return Err("original artifact changed during audio acceptance".into());
    }
    fs::write(
        base.join("result.json"),
        serde_json::to_vec_pretty(
            &json!({"scope":kind,"source_sha":commit,"architecture":architecture,"artifact_sha256":receipt.artifact_sha256,"positive":results,"silent_disabled":true,"missing_member_negatives":3,"general_controls":false,"native_source_acceptance":"separate per-artifact inventory/source verifier gate"}),
        )?,
    )?;
    println!("Captured packaged audio ({kind}, {architecture}); evidence: {}. General controls and source acceptance are separate.",base.display());
    Ok(())
}

/// reuse an already checked extraction, but bind the fault probe independently
/// to the same actual artifact/source. No test interposer is shipped in it.
pub fn retirement(
    prepared: &Path,
    artifact: &Path,
    checkers: &Path,
    commit: &str,
    architecture: &str,
    output: &Path,
) -> Result<()> {
    audio_capture::require_container()?;
    let receipt = validate(checkers, artifact, commit, architecture)?;
    let identity = storage::parse_json(&fs::read(prepared.join("identity.json"))?)?;
    if identity["receipt"]["artifact_sha256"] != receipt.artifact_sha256
        || identity["receipt"]["source_sha"] != commit
        || identity["receipt"]["architecture"] != architecture
    {
        return Err("retirement fixture differs from checked package source/architecture".into());
    }
    let scope = identity["scope"]
        .as_str()
        .ok_or("prepared audio scope missing")?;
    let payload = match scope {
        "appimage" => prepared.join("squashfs-root"),
        "snap-core24" => prepared.join("snap-payload"),
        "snap-installed" => PathBuf::from("/snap/among/current"),
        _ => return Err("unsupported retirement package scope".into()),
    }
    .canonicalize()?;
    let lib = payload.join(if scope == "appimage" {
        "usr/lib".to_string()
    } else {
        format!(
            "usr/lib/{}",
            if architecture == "amd64" {
                "x86_64-linux-gnu"
            } else {
                "aarch64-linux-gnu"
            }
        )
    });
    let mut observed = BTreeMap::new();
    manifest(&lib, &lib, &mut observed)?;
    if serde_json::to_value(&observed)?
        != storage::parse_json(&fs::read(prepared.join("native-files.json"))?)?
    {
        return Err("prepared native payload changed before retirement testing".into());
    }
    fs::create_dir(output)?;
    let base = output.canonicalize()?;
    let probe = base.join("retirement-probe");
    fs::copy(checkers.join("audio-probe"), &probe)?;
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o755))?;
    let app = payload.join(if scope == "appimage" {
        "usr/bin/among"
    } else {
        "bin/among"
    });
    let origin = utf8(app.parent().ok_or("application origin missing")?)?;
    let runpath = command(
        "/usr/bin/patchelf",
        &["--print-rpath", utf8(&app)?],
        &base,
        "application-runpath",
    )?
    .replace("${ORIGIN}", origin)
    .replace("$ORIGIN", origin);
    command(
        "/usr/bin/patchelf",
        &["--set-rpath", &runpath, utf8(&probe)?],
        &base,
        "probe-runpath",
    )?;
    if scope != "appimage" {
        let interpreter = command(
            "/usr/bin/patchelf",
            &["--print-interpreter", utf8(&app)?],
            &base,
            "application-interpreter",
        )?;
        if !interpreter.starts_with("/snap/core24/") {
            return Err("retirement test lacks actual core24 interpreter".into());
        }
        command(
            "/usr/bin/patchelf",
            &["--set-interpreter", &interpreter, utf8(&probe)?],
            &base,
            "probe-interpreter",
        )?;
    }
    let input = prepared.join("fixtures/1-16.wav").canonicalize()?;
    let mut results = Vec::new();
    let positive = [
        "stall-construct-cancel",
        "stall-retire-shutdown",
        "stall-event-revoke",
        "stall-source-replace",
        "stall-owner-drop",
        "stall-watchdog",
        "helper-crash",
        "stall-quarantine",
        "late-expiry",
    ];
    let cancellations = [
        "cancel-before-spawn",
        "cancel-late-spawn",
        "cancel-constructor",
        "cancel-pending-permit",
        "cancel-late-spawn-event-revoke",
        "cancel-late-spawn-source-replace",
        "cancel-late-spawn-shutdown",
        "cancel-late-spawn-owner-drop",
    ];
    let cases: Vec<_> = positive
        .into_iter()
        .map(|case| (case.to_string(), true))
        .chain(cancellations.into_iter().flat_map(|case| {
            [
                (case.to_string(), false),
                (format!("{case}-recovery"), true),
            ]
        }))
        .collect();
    for (scenario, expected_signal) in cases {
        let evidence = base.join(&scenario);
        let signal = audio_capture::run(
            &Invocation {
                probe: &probe,
                scenario: &scenario,
                input: &input,
                payload: &payload,
                installed: scope == "snap-installed",
                expected_failure: None,
                signal: expected_signal,
            },
            &evidence,
        )?;
        let measurements =
            storage::parse_json(&fs::read(evidence.join("consumer/retirement.json"))?)?;
        if measurements["scenario"] != scenario
            || measurements["supervisor_threads"] != 1
            || measurements["operation_temp_dirs_after"] != 0
            || measurements["max_gtk_gap_ms"]
                .as_u64()
                .is_none_or(|gap| gap >= 250)
            || measurements["gui_latency_us"]
                .as_u64()
                .is_none_or(|latency| latency >= 100_000)
        {
            return Err("retirement liveness measurements violated the contract".into());
        }
        if expected_signal {
            maps(&payload, &evidence)?;
        }
        let separated_output = if scenario == "stall-retire-shutdown" {
            Some(audio_capture::recovered_output(&fs::read(
                evidence.join("capture.s16le"),
            )?)?)
        } else {
            None
        };
        results.push(json!({"scenario":scenario,"capture":signal,"measurements":measurements,"separated_output":separated_output}));
    }
    if digest(artifact)? != receipt.artifact_sha256 {
        return Err("actual package changed during retirement tests".into());
    }
    fs::write(
        base.join("result.json"),
        serde_json::to_vec_pretty(
            &json!({"source_sha":commit,"artifact_sha256":receipt.artifact_sha256,"scope":scope,"architecture":architecture,"injection":"trusted native-call and signal-failure interposers in external probe only","cases":results,"general_controls":false}),
        )?,
    )?;
    println!(
        "Captured decoder retirement/GTK heartbeat and real output after recovery: {}",
        base.display()
    );
    Ok(())
}
