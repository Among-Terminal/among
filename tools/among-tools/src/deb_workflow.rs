use crate::{deb_build, process::OwnedProcess, storage, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn require(value: bool, message: &str) -> Result<()> {
    if value {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn matrix(job: &Value, build: bool) -> Result<BTreeSet<(String, String, String, String)>> {
    let rows = job["strategy"]["matrix"]["include"]
        .as_array()
        .ok_or("native matrix is missing")?;
    let mut targets = BTreeSet::new();
    let mut records = BTreeSet::new();
    for row in rows {
        let target = row["target"].as_str().ok_or("matrix target missing")?;
        let arch = row["architecture"]
            .as_str()
            .ok_or("matrix architecture missing")?;
        let runner = row["runner"].as_str().ok_or("matrix runner missing")?;
        let image = row["image"].as_str().ok_or("matrix image missing")?;
        require(
            matches!(arch, "amd64" | "arm64"),
            "unsupported native workflow architecture",
        )?;
        deb_build::image_identity(image, target)?;
        require(
            runner
                == if arch == "arm64" {
                    "ubuntu-24.04-arm"
                } else {
                    "ubuntu-24.04"
                },
            "workflow does not select target-native hosted worker",
        )?;
        if build {
            require(
                row["rust_host"]
                    == if arch == "arm64" {
                        "aarch64-unknown-linux-gnu"
                    } else {
                        "x86_64-unknown-linux-gnu"
                    },
                "workflow Rust host differs from native target",
            )?;
        }
        require(
            targets.insert((target.to_string(), arch.to_string())),
            "duplicate native workflow target",
        )?;
        records.insert((target.into(), arch.into(), runner.into(), image.into()));
    }
    require(
        targets
            == ["debian13", "ubuntu24.04"]
                .into_iter()
                .flat_map(|target| {
                    ["amd64", "arm64"].map(|arch| (target.to_string(), arch.to_string()))
                })
                .collect(),
        "release workflow must cover exactly four native targets",
    )?;
    Ok(records)
}

pub fn validate(document: &Value) -> Result<Vec<String>> {
    require(
        document["on"] == json!({"push":{"tags":["v*"]}}),
        "Debian workflow must remain release-tag-only",
    )?;
    require(
        document["permissions"] == json!({"contents":"read"}),
        "Debian workflow must not gain publication permissions",
    )?;
    require(
        document["concurrency"]["cancel-in-progress"] == false,
        "release workflow must not cancel native builds",
    )?;
    let build = &document["jobs"]["build"];
    let installed = &document["jobs"]["installed"];
    require(
        installed["needs"] == "build" && installed["container"] == build["container"],
        "installed job must depend on build and match its container selection",
    )?;
    require(
        matrix(build, true)? == matrix(installed, false)?,
        "installed and build native targets differ",
    )?;
    let mut scripts = Vec::new();
    let mut commands = Vec::new();
    for job in [build, installed] {
        let steps = job["steps"].as_array().ok_or("job steps missing")?;
        let mut runs = Vec::new();
        for step in steps {
            if let Some(action) = step.get("uses") {
                let (name, sha) = action
                    .as_str()
                    .and_then(|action| action.split_once('@'))
                    .ok_or("invalid action reference")?;
                require(
                    !name.is_empty()
                        && name
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"_/-".contains(&byte))
                        && sha.len() == 40
                        && sha
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')),
                    "workflow action must be SHA-pinned",
                )?;
            }
            if let Some(run) = step.get("run") {
                let run = run.as_str().ok_or("workflow command is not text")?;
                runs.push(run.to_string());
                scripts.push(run.to_string());
            }
        }
        commands.push(runs.join("\n"));
    }
    require(
        commands[0].contains("ci-deb-build . \"$BUILD_TARGET\"")
            && commands[0].contains("\"$BUILD_ARCH\"")
            && commands[0].contains("--default-toolchain 1.98.1")
            && commands[0].contains("--test binary_archive"),
        "native builder dispatch/pinned Rust missing",
    )?;
    require(
        commands[0].contains("useradd --create-home --shell /bin/bash among-build")
            && commands[0].contains("chown among-build:among-build /tmp/opencode"),
        "native package tests require a dedicated unprivileged build user and owned test scratch",
    )?;
    require(
        !commands[0].contains("gh release") && !commands[0].contains("snapcraft upload"),
        "build workflow must not publish",
    )?;
    require(
        commands[1].contains("native-checker/among-tools ci-deb-install . \"$BUILD_TARGET\"")
            && commands[1].contains("sha256sum --check native-checker.sha256")
            && commands[1].contains("check-native-checker")
            && !commands[1].contains("python3"),
        "source-bound prebuilt installed acceptance dispatch missing",
    )?;
    require(
        !commands[1].contains("rustup-init") && !commands[1].contains("-dev "),
        "installed acceptance must not install compiler/headers",
    )?;
    Ok(scripts)
}

pub fn read(text: &str) -> Result<Value> {
    let mut yaml: serde_yaml_ng::Value = serde_yaml_ng::from_str(text)?;
    yaml.apply_merge()?;
    Ok(serde_json::to_value(yaml)?)
}

pub fn run(root: &Path, scratch: &Path) -> Result<()> {
    let path = storage::safe_path(root, Path::new(".github/workflows/deb-release.yml"))?;
    let scripts = validate(&read(&fs::read_to_string(path)?)?)?;
    let base = scratch.join(format!(
        "among-deb-workflow-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::create_dir(&base)?;
    let env = [("PATH", "/usr/bin:/bin")]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    for (index, script) in scripts.iter().enumerate() {
        let log = File::create(base.join(format!("syntax-{index}.log")))?;
        let mut child =
            OwnedProcess::spawn_interactive(Path::new("/bin/bash"), &["-n"], &env, &base, &log)?;
        let mut stdin = child.take_stdin()?;
        stdin.write_all(script.as_bytes())?;
        drop(stdin);
        if !child.wait(Duration::from_secs(10))?.success() {
            return Err("release workflow shell syntax failed".into());
        }
    }
    println!(
        "PASS release-only four-target build/installed workflow policy and {} shell syntax checks",
        scripts.len()
    );
    Ok(())
}
