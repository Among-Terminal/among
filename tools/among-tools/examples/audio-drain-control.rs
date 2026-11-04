//! One matched third-party command-boundary experiment, not release acceptance.
use among_tools::{audio_payload, storage};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
type Result<T> = among_tools::Result<T>;
const FIXTURE: &str = "64bada62d3a4e674af1d4af818c9517ab51cfff7927422cff3ea6b0de82a9874";
const MODULE: &str = "usr/lib/gstreamer-1.0/libgstpulseaudio.so";

fn digest(path: &Path) -> Result<String> {
    let file =
        File::open(path).map_err(|error| format!("open hash input {}: {error}", path.display()))?;
    storage::sha256(file)
}
fn copy(source: &Path, destination: &Path) -> Result<()> {
    if destination.exists() {
        return Err(format!("refusing existing output {}", destination.display()).into());
    }
    let parent = destination.parent().ok_or("destination parent absent")?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("create copy parent {}: {error}", parent.display()))?;
    fs::copy(source, destination).map_err(|error| {
        format!(
            "copy {} -> {}: {error}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}
fn fixtures(root: &Path) -> Result<()> {
    for name in ["tone-eight.wav", "tone-recovery.wav"] {
        if digest(&root.join(name))? != FIXTURE {
            return Err(format!("retained fixture identity mismatch: {name}").into());
        }
        if fs::metadata(root.join(name))?.len() != 768044 {
            return Err("retained fixture size differs".into());
        }
    }
    Ok(())
}
fn prepare_payload(payload: &Path, repository: &Path) -> Result<Value> {
    let triplet = match std::env::consts::ARCH {
        "x86_64" => "x86_64-linux-gnu",
        "aarch64" => "aarch64-linux-gnu",
        _ => return Err("native architecture unsupported".into()),
    };
    let native = PathBuf::from(format!("/usr/lib/{triplet}"));
    audio_payload::require(&native)
        .map_err(|error| format!("required native audio inputs {}: {error}", native.display()))?;
    fs::create_dir(payload)
        .map_err(|error| format!("create new payload {}: {error}", payload.display()))?;
    let destination_lib = payload.join("usr/lib");
    // safe_path intentionally requires an existing real root; the caller must
    // establish it before the shared payload copier validates descendants.
    fs::create_dir_all(&destination_lib).map_err(|error| {
        format!(
            "create payload library root {}: {error}",
            destination_lib.display()
        )
    })?;
    copy(
        &repository.join("target/release/among"),
        &payload.join("usr/bin/among"),
    )?;
    audio_payload::stage(&native, &destination_lib).map_err(|error| {
        format!(
            "stage native audio {} -> {}: {error}",
            native.display(),
            destination_lib.display()
        )
    })?;
    copy(
        &native.join("libgstreamer-1.0.so.0"),
        &destination_lib.join("libgstreamer-1.0.so.0"),
    )?;
    let mut files = BTreeMap::new();
    files.insert(
        "usr/bin/among".to_string(),
        digest(&payload.join("usr/bin/among"))?,
    );
    files.insert(
        "usr/lib/libgstreamer-1.0.so.0".to_string(),
        digest(&destination_lib.join("libgstreamer-1.0.so.0"))?,
    );
    for relative in audio_payload::MEMBERS {
        let relative = format!("usr/lib/{relative}");
        files.insert(relative.clone(), digest(&payload.join(relative))?);
    }
    Ok(json!({"native_library_directory":native,"files":files}))
}
fn preflight(repository: &Path, payload: &Path) -> Result<()> {
    let receipt = prepare_payload(payload, repository)?;
    fs::write(
        payload.join("preflight-receipt.json"),
        serde_json::to_vec_pretty(
            &json!({"scope":"NATIVE_PAYLOAD_IO_PREFLIGHT_ONLY","actual_copied_inputs":receipt,"producer_executed":false,"matched_C_builds_executed":false}),
        )?,
    )?;
    Ok(())
}
fn stage(root: &Path, repository: &Path, baseline: &Path, candidate: &Path) -> Result<()> {
    fs::create_dir(root)
        .map_err(|error| format!("create matched payload root {}: {error}", root.display()))?;
    let mut receipts = BTreeMap::new();
    for (name, module) in [("baseline", baseline), ("candidate", candidate)] {
        let payload = root.join(name);
        let mut receipt = prepare_payload(&payload, repository)?;
        let destination = payload.join(MODULE);
        fs::copy(module, &destination).map_err(|error| {
            format!(
                "replace selected producer module {} -> {}: {error}",
                module.display(),
                destination.display()
            )
        })?;
        let actual = digest(&destination)?;
        if actual != digest(module)? {
            return Err("selected module copy hash differs".into());
        }
        receipt["files"][MODULE] = json!(actual);
        receipts.insert(name, receipt);
    }
    let mut unchanged = receipts["baseline"]["files"]
        .as_object()
        .ok_or("baseline files absent")?
        .clone();
    unchanged.remove(MODULE);
    let mut comparison = receipts["candidate"]["files"]
        .as_object()
        .ok_or("candidate files absent")?
        .clone();
    comparison.remove(MODULE);
    if unchanged != comparison {
        return Err("unexpected non-module payload difference".into());
    }
    // These are experimental source-bound payloads. Unlike full package gates,
    // this experiment does not claim that all native dependencies are bundled.
    fs::write(
        root.join("control-scope.json"),
        serde_json::to_vec_pretty(
            &json!({"scope":"MATCHED_PRODUCER_COMMAND_BOUNDARY_ONLY", "baseline_module":digest(baseline)?, "candidate_module":digest(candidate)?, "shipped_among":digest(&repository.join("target/release/among"))?, "actual_payloads":receipts, "package_acceptance":false}),
        )?,
    )?;
    Ok(())
}
fn value(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn maps(root: &Path, payload: &Path) -> Result<Value> {
    let mut objects = BTreeMap::new();
    for relative in ["server-maps.txt", "consumer/decoder-maps.txt"] {
        let contents = fs::read_to_string(root.join(relative))?;
        if contents.is_empty() || contents.contains("libserver_") {
            return Err("missing maps or native observer mapped".into());
        }
        if relative.starts_with("consumer/")
            && !contents.contains(payload.join(MODULE).to_str().ok_or("non-UTF8 payload")?)
        {
            return Err("selected producer module not observed".into());
        }
        for line in contents.lines() {
            if let Some(name) = line
                .split_whitespace()
                .last()
                .filter(|name| name.starts_with('/'))
            {
                let path = Path::new(name);
                if path.is_file() {
                    objects.insert(name.to_string(), digest(path)?);
                }
            }
        }
    }
    Ok(serde_json::to_value(objects)?)
}
fn process_scope(root: &Path) -> Result<Value> {
    let mut records = BTreeMap::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry
            .file_name()
            .as_encoded_bytes()
            .ends_with(b".process.json")
        {
            records.insert(
                entry.file_name().to_string_lossy().into_owned(),
                value(&entry.path())?,
            );
        }
    }
    Ok(
        json!({"capture_process_receipts":records,"actual_helper_identity":fs::read_to_string(root.join("consumer/actual-executable.txt"))?}),
    )
}
fn orphaned_native_processes() -> Result<Vec<String>> {
    let uid = fs::metadata("/proc/self")?.uid();
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc")? {
        let path = entry?.path();
        if !path
            .file_name()
            .is_some_and(|name| name.as_encoded_bytes().iter().all(u8::is_ascii_digit))
        {
            continue;
        }
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if metadata.uid() != uid {
            continue;
        }
        let bytes = match fs::read(path.join("cmdline")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let executable = bytes.split(|&byte| byte == 0).next().unwrap_or_default();
        let name = Path::new(std::ffi::OsStr::from_bytes(executable));
        if [
            "pulseaudio",
            "parec",
            "Xvfb",
            "among",
            "audio-shipped-control",
        ]
        .iter()
        .any(|expected| name.file_name().is_some_and(|name| name == *expected))
        {
            found.push(format!(
                "{} {}",
                path.display(),
                String::from_utf8_lossy(&bytes).replace('\0', " ")
            ));
        }
    }
    Ok(found)
}
fn run(repository: &Path, payloads: &Path, input: &Path, evidence: &Path) -> Result<()> {
    fixtures(input)?;
    fs::create_dir(evidence)?;
    let issuer = repository.join("target/release/examples/audio-shipped-control");
    let mut results = Vec::new();
    let mut rejected = false;
    let mut baseline_reproduced = false;
    for (verbosity, executable) in [
        ("diagnostic", "audio-drain-capture"),
        ("none", "audio-effect-capture"),
    ] {
        for build in ["baseline", "candidate"] {
            let payload = payloads.join(build).canonicalize()?;
            for scenario in ["actual-normal", "actual-active-revoke"] {
                let name = format!("{verbosity}-{build}-{scenario}");
                let root = evidence.join(&name);
                let status =
                    Command::new(repository.join("target/release/examples").join(executable))
                        .args([
                            "default".as_ref(),
                            issuer.as_os_str(),
                            scenario.as_ref(),
                            input.join("tone-eight.wav").as_os_str(),
                            payload.as_os_str(),
                            root.as_os_str(),
                            "signal".as_ref(),
                        ])
                        .env_remove("LD_PRELOAD")
                        .env_remove("GST_DEBUG")
                        .stdin(Stdio::null())
                        .stdout(File::create_new(evidence.join(format!("{name}.stdout")))?)
                        .stderr(File::create_new(evidence.join(format!("{name}.stderr")))?)
                        .status()?;
                let orphans = orphaned_native_processes()?;
                fs::write(
                    evidence.join(format!("{name}-process-scope.json")),
                    serde_json::to_vec_pretty(
                        &json!({"known_owned_scope":process_scope(&root)?,"same_uid_known_name_survivors":orphans,"signals_sent_by_scan":[],"complete_production_timeout_or_concurrency_acceptance":false}),
                    )?,
                )?;
                if !orphans.is_empty() {
                    return Err(
                        "same-UID native process remains; refusing contaminated subsequent cells"
                            .into(),
                    );
                }
                let observation = (|| -> Result<Value> {
                    let loaded_objects = maps(&root, &payload)?;
                    let producer_log =
                        fs::read_to_string(root.join("consumer/actual-production-stderr.txt"))?;
                    let reached = producer_log
                        .contains("Final-context pending-dispatch drain requested")
                        && producer_log
                            .contains("Final-context pending-dispatch drain callback reached")
                        && producer_log.contains("Final-context drain finished with state 1");
                    if build == "candidate" && verbosity == "diagnostic" && !reached {
                        return Err("candidate drain branch/DONE callback not established".into());
                    }
                    if !status.success() {
                        let server = value(&root.join("server.exit.json"))?;
                        let log = fs::read_to_string(root.join("server.stderr"))?;
                        let family = server["signals_actually_sent"]
                            .as_array()
                            .is_some_and(|signals| signals.is_empty())
                            && server["status"]
                                .as_str()
                                .is_some_and(|status| status.contains("SIGABRT"))
                            && log.contains("memblock_replace_import()")
                            && (log.contains("memblock.c:780") || log.contains("memblock.c:781"))
                            && root.join("INVALID.txt").is_file();
                        if build == "baseline" && scenario == "actual-normal" && family {
                            if verbosity == "none" {
                                baseline_reproduced = true;
                            }
                            return Ok(
                                json!({"capture_passed":false,"retained_invalid":true,"import_accounting_abort_family":true,"loaded_objects":loaded_objects}),
                            );
                        }
                        return Err("actual capture failed; retained INVALID stays failed".into());
                    }
                    let result = value(&root.join("result.json"))?;
                    if result["valid"] != true {
                        return Err("strict capture verdict absent".into());
                    }
                    Ok(
                        json!({"capture_passed":true,"branch_reached":reached,"capture":result,"loaded_objects":loaded_objects}),
                    )
                })();
                if observation.is_err() {
                    rejected = true;
                }
                results.push(json!({"case":name,"exit":status.to_string(),"observation":observation.as_ref().ok(),"error":observation.err().map(|error|error.to_string()),"package_acceptance":false}));
                fs::write(
                    evidence.join("results.json"),
                    serde_json::to_vec_pretty(&results)?,
                )?;
            }
        }
    }
    fixtures(input)?;
    fs::write(
        evidence.join("scope.json"),
        serde_json::to_vec_pretty(
            &json!({"unexpected_control_failure":rejected,"baseline_uninstrumented_abort_family_reproduced":baseline_reproduced,"contrast":if rejected {"REJECTED_CONTROL"} else if baseline_reproduced {"BASELINE_ABORT_VS_CANDIDATE_PASS_REQUIRES_CAUSAL_REVIEW"} else {"NOT_ESTABLISHED_BASELINE_DID_NOT_ABORT"},"product_fix_accepted":false,"production_timeout_and_concurrency":"NOT_RUN", "package_acceptance":false}),
        )?,
    )?;
    if rejected {
        return Err("unexpected matched-control failure; raw INVALID stays failed".into());
    }
    Ok(())
}
fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args_os().collect();
    match arguments.get(1).and_then(|value|value.to_str()) {
        Some("fixtures") if arguments.len() == 3 => fixtures(Path::new(&arguments[2])),
        Some("preflight") if arguments.len() == 4 => preflight(Path::new(&arguments[2]), Path::new(&arguments[3])),
        Some("stage") if arguments.len() == 6 => stage(Path::new(&arguments[2]), Path::new(&arguments[3]), Path::new(&arguments[4]), Path::new(&arguments[5])),
        Some("run") if arguments.len() == 6 => run(Path::new(&arguments[2]), Path::new(&arguments[3]), Path::new(&arguments[4]), Path::new(&arguments[5])),
        _ => Err("fixtures DIR | preflight REPOSITORY OUT | stage OUT REPOSITORY BASELINE_SO CANDIDATE_SO | run REPOSITORY PAYLOADS FIXTURES EVIDENCE".into()),
    }
}
