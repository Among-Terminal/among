use crate::plugin_install::release;
use crate::process::{allowed_environment, OwnedProcess};
use crate::{storage, Result};
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub fn cpu_counters(text: &str) -> Result<Vec<u64>> {
    let row = text
        .lines()
        .find(|line| line.starts_with("cpu "))
        .ok_or("missing aggregate CPU accounting")?;
    let counters = row
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(str::parse)
        .collect::<std::result::Result<Vec<u64>, _>>()?;
    if counters.len() != 8 {
        return Err("CPU accounting requires eight counters".into());
    }
    Ok(counters)
}

pub fn cpu_percent(before: &[u64], after: &[u64]) -> Result<f64> {
    if before.len() != 8 || after.len() != 8 {
        return Err("CPU samples require eight counters".into());
    }
    let deltas = before
        .iter()
        .zip(after)
        .map(|(start, end)| end.checked_sub(*start).ok_or("CPU counter moved backwards"))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let total = deltas.iter().try_fold(0u64, |total, next| {
        total.checked_add(*next).ok_or("CPU delta overflow")
    })?;
    if total == 0 {
        return Err("CPU interval has no accounting delta".into());
    }
    Ok(100.0 * (total - deltas[3] - deltas[4]) as f64 / total as f64)
}

pub fn memory_percent(text: &str) -> Result<f64> {
    let mut total = None;
    let mut available = None;
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let destination = match fields.next() {
            Some("MemTotal:") => &mut total,
            Some("MemAvailable:") => &mut available,
            _ => continue,
        };
        let value = fields
            .next()
            .ok_or("missing memory accounting value")?
            .parse::<u64>()?;
        if fields.next() != Some("kB")
            || fields.next().is_some()
            || destination.replace(value).is_some()
        {
            return Err("malformed or duplicate memory accounting".into());
        }
    }
    let total = total.ok_or("missing MemTotal")?;
    let available = available.ok_or("missing MemAvailable")?;
    if total == 0 || available > total {
        return Err("invalid memory accounting range".into());
    }
    Ok(100.0 * (total - available) as f64 / total as f64)
}

pub fn disk_percent(text: &str) -> Result<f64> {
    let row = text.lines().last().ok_or("df emitted no accounting")?;
    let fields = row.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 6 || fields.last() != Some(&"/") {
        return Err("df output does not describe root mount".into());
    }
    Ok(fields[fields.len() - 2]
        .strip_suffix('%')
        .ok_or("df percentage lacks suffix")?
        .parse::<u32>()? as f64)
}

/// Identity, session and generation the check presents in its managed
/// handshake; every reply must carry the same binding back.
const IDENTITY: &str = "github:1431055733/system-status";
const SESSION: &str = "resource-check-session";
const GENERATION: &str = "resource-check-generation";

/// Features the plugin needs to run at all, and the larger set the check
/// grants in its hello: the plugin answers with the part of it that it uses.
const REQUIRED_FEATURES: [&str; 2] = ["status", "system.metrics"];
const GRANTED_FEATURES: [&str; 5] = [
    "status",
    "system.metrics",
    "settings",
    "context.terminals",
    "context.cwd",
];

fn hello() -> Value {
    json!({"type":"managed_hello","major":1,"minor":2,"identity":IDENTITY,
        "session":SESSION,"generation":GENERATION,"features":GRANTED_FEATURES})
}

/// Whether a plugin's hello echo admits it, as the host decides: same
/// protocol, identity and binding, and features that are a subset of what was
/// granted and still hold every required feature.
pub fn hello_reply_accepted(reply: &Value) -> bool {
    let sent = hello();
    [
        "type",
        "major",
        "minor",
        "identity",
        "session",
        "generation",
    ]
    .iter()
    .all(|key| reply[*key] == sent[*key])
        && reply["features"].as_array().is_some_and(|features| {
            features.iter().all(|feature| {
                feature
                    .as_str()
                    .is_some_and(|f| GRANTED_FEATURES.contains(&f))
            }) && REQUIRED_FEATURES
                .iter()
                .all(|required| features.iter().any(|feature| feature == required))
        })
}

pub fn response(bytes: &[u8], id: u64) -> Result<Vec<Value>> {
    let value = storage::parse_json(bytes)?;
    if value["type"] != "status" || value["session"] != SESSION || value["generation"] != GENERATION
    {
        return Err("plugin response is not a status reply bound to this session".into());
    }
    if value["request_id"].as_u64() != Some(id) {
        return Err("plugin response request ID differs".into());
    }
    let fragments = value["fragments"]
        .as_array()
        .ok_or("plugin response lacks fragments")?;
    if fragments.len() != 3 {
        return Err("resource response requires CPU/MEM/DISK fragments".into());
    }
    Ok(fragments.clone())
}

pub fn compare(fragments: &[Value], independent: &[f64; 3]) -> Result<[u32; 3]> {
    if fragments.len() != 3 {
        return Err("resource response requires three fragments".into());
    }
    let mut actual = [0; 3];
    for (index, fragment) in fragments.iter().enumerate() {
        let text = fragment["text"]
            .as_str()
            .ok_or("resource fragment lacks text")?;
        let number = text
            .split_whitespace()
            .nth(1)
            .ok_or("resource text lacks percentage")?
            .strip_suffix('%')
            .ok_or("resource text lacks percent suffix")?
            .parse::<u32>()?;
        let tolerance = [2.0, 2.0, 1.0][index];
        if !independent[index].is_finite()
            || (f64::from(number) - independent[index]).abs() > tolerance
        {
            return Err(format!(
                "resource {index} differs: plugin={number}, independent={}, tolerance={tolerance}",
                independent[index]
            )
            .into());
        }
        if fragment["meter"].as_f64() != Some(f64::from(number) / 100.0) {
            return Err("fragment meter differs from displayed percentage".into());
        }
        actual[index] = number;
    }
    Ok(actual)
}

fn request(stdin: &mut impl Write, stdout: &mut impl Read, id: u64) -> Result<Vec<Value>> {
    writeln!(
        stdin,
        "{}",
        json!({"type":"status","session":SESSION,"generation":GENERATION,"request_id":id,
            "scope":{"slot":"app_header","target":{"kind":"application"}},
            "context":{"session":SESSION}})
    )?;
    stdin.flush()?;
    let line = read_line(stdout)?;
    response(&line, id)
}

/// Reads one newline-terminated frame within five seconds and one MiB.
fn read_line(stdout: &mut impl Read) -> Result<Vec<u8>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut line = Vec::new();
    loop {
        let mut byte = [0];
        match stdout.read(&mut byte) {
            Ok(0) => return Err("plugin closed output before its response".into()),
            Ok(_) if byte[0] == b'\n' => return Ok(line),
            Ok(_) => {
                line.push(byte[0]);
                if line.len() > 1024 * 1024 {
                    return Err("plugin response exceeds one MiB".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Err(error) => return Err(error.into()),
        }
        if Instant::now() >= deadline {
            return Err("plugin resource request timed out".into());
        }
    }
}

/// The installed system-status executable of an observed official-plugin
/// profile, accepted only when it is the exact catalogue artifact: the receipt,
/// the manifest digest, the artifact digest and the executable's own digest
/// and size must all agree with the validated release catalogue.
pub fn pinned_executable(profile: &Path) -> Result<PathBuf> {
    fn bounded(root: &Path, relative: &str, limit: u64) -> Result<Vec<u8>> {
        let path = storage::safe_path(root, Path::new(relative))?;
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.len() > limit {
            return Err(format!("{relative} is not a bounded regular file").into());
        }
        Ok(fs::read(path)?)
    }
    let text = |value: &Value, key: &str| -> Result<String> {
        Ok(value[key]
            .as_str()
            .ok_or_else(|| format!("profile record lacks {key}"))?
            .to_owned())
    };
    let inputs = release::profile_inputs(profile)?;
    let catalogue = release::catalogue(profile, &inputs)?;
    let architecture = crate::platform::discover()?.architecture;
    let (version, artifact) = release::select(&catalogue, &architecture)?
        .into_iter()
        .find(|(version, _)| version["manifest"]["id"] == "system-status")
        .ok_or("the release catalogue holds no system-status artifact")?;
    // The observation entry binds this profile to the release it was taken
    // from and fixes the bytes of the selection; a bare directory tree without
    // it is not an observed installation.
    let observation = storage::parse_json(&bounded(profile, "profile.json", 64 * 1024)?)?;
    if observation["kind"] != "observed_published_installation_profile"
        || observation["source_revision"] != inputs["source_revision"]
        || observation["catalogue_revision"] != inputs["catalogue_revision"]
        || observation["architecture"] != architecture.as_str()
    {
        return Err("the profile observation differs from the release it names".into());
    }
    let managed = storage::safe_path(profile, Path::new("managed"))?;
    let selection = bounded(&managed, "installed.json", 256 * 1024)?;
    if observation["installed_size"] != selection.len()
        || observation["installed_sha256"] != storage::sha256(selection.as_slice())?.as_str()
    {
        return Err("the installed selection differs from the observed profile".into());
    }
    let installed = storage::parse_json(&selection)?;
    let receipts = installed["installations"]
        .as_array()
        .ok_or("installed selection lacks installations")?
        .iter()
        .filter(|receipt| {
            receipt["repository_id"] == 1431055733u64 && receipt["plugin_id"] == "system-status"
        })
        .collect::<Vec<_>>();
    let [receipt] = receipts.as_slice() else {
        return Err("system-status selection is absent or duplicated".into());
    };
    let generation = text(receipt, "generation")?;
    if generation.len() != 64 || !generation.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid system-status generation".into());
    }
    let package = format!("generations/1431055733/system-status/{generation}");
    let manifest = bounded(
        &managed,
        &format!("{package}/among-plugin.json"),
        256 * 1024,
    )?;
    let manifest_digest = storage::sha256(manifest.as_slice())?;
    if manifest_digest != text(receipt, "manifest_sha256")?
        || artifact["sha256"] != receipt["package_sha256"]
    {
        return Err("installed system-status differs from its selected receipt".into());
    }
    // The receipt is the installer's own record; the catalogue is the reviewed
    // release, so the manifest must also be the one the catalogue declares.
    if manifest_digest != text(version, "manifest_sha256")? {
        return Err("installed system-status manifest differs from the catalogue".into());
    }
    let manifest = storage::parse_json(&manifest)?;
    let entrypoint = text(&manifest, "entrypoint")?;
    let member = artifact["members"]
        .as_array()
        .ok_or("artifact members absent")?
        .iter()
        .find(|member| member["path"] == entrypoint.as_str())
        .ok_or("the entrypoint has no public digest")?;
    let path = storage::safe_path(&managed, Path::new(&format!("{package}/{entrypoint}")))?;
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o111 == 0
        || member["executable"] != true
        || member["size"] != metadata.len()
        || storage::sha256(File::open(&path)?)? != text(member, "sha256")?
    {
        return Err("system-status executable differs from the catalogue artifact".into());
    }
    Ok(path)
}

/// Measures the system-status executable that an observed official-plugin
/// profile installed, after pinning it to the catalogue artifact.
pub fn run(profile: &Path, scratch: &Path) -> Result<Value> {
    measure(&pinned_executable(profile)?, scratch)
}

pub fn measure(binary: &Path, scratch: &Path) -> Result<Value> {
    let binary = binary.canonicalize()?;
    fs::create_dir_all(scratch)?;
    let base = storage::private_directory(scratch, "among-resource-check")?;
    let log = File::create(base.join("plugin.log"))?;
    let mut env = allowed_environment(&std::env::vars_os().collect(), &["PATH"]);
    env.insert("HOME".into(), base.clone().into_os_string());
    env.insert("LANG".into(), "C.UTF-8".into());
    let mut plugin = OwnedProcess::spawn_interactive(&binary, &[], &env, &base, &log)?;
    let mut stdin = plugin.take_stdin()?;
    let mut stdout = plugin.take_stdout()?;
    let flags = OFlag::from_bits_truncate(fcntl(&stdout, FcntlArg::F_GETFL)?);
    fcntl(&stdout, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    writeln!(stdin, "{}", hello())?;
    stdin.flush()?;
    if !hello_reply_accepted(&storage::parse_json(&read_line(&mut stdout)?)?) {
        return Err("plugin did not admit itself with the required features".into());
    }
    let start = Instant::now();
    let before = cpu_counters(&fs::read_to_string("/proc/stat")?)?;
    let first = request(&mut stdin, &mut stdout, 1)?;
    if first[0]["text"] != "CPU N/A" || !first[0]["meter"].is_null() {
        return Err("first CPU response must be N/A without meter".into());
    }
    std::thread::sleep(Duration::from_millis(1100));
    let after = cpu_counters(&fs::read_to_string("/proc/stat")?)?;
    let measured = request(&mut stdin, &mut stdout, 2)?;
    let interval = start.elapsed().as_secs_f64();
    let df_path = base.join("df.log");
    let df_log = File::create(&df_path)?;
    let mut df = OwnedProcess::spawn(Path::new("/usr/bin/df"), &["-P", "/"], &env, &base, &df_log)?;
    if !df.wait(Duration::from_secs(5))?.success() {
        return Err("independent df measurement failed".into());
    }
    let independent = [
        cpu_percent(&before, &after)?,
        memory_percent(&fs::read_to_string("/proc/meminfo")?)?,
        disk_percent(&fs::read_to_string(df_path)?)?,
    ];
    let actual = compare(&measured, &independent)?;
    writeln!(stdin, "{}", json!({"type":"shutdown"}))?;
    stdin.flush()?;
    if !plugin.wait(Duration::from_secs(5))?.success() {
        return Err("resource plugin shutdown failed".into());
    }
    let evidence = json!({"interval_seconds":interval, "plugin_percent":actual, "independent_percent":independent, "tolerance_percentage_points":[2,2,1], "root_mount":"/"});
    storage::atomic_write(
        &base,
        Path::new("measurements.json"),
        &serde_json::to_vec_pretty(&evidence)?,
        0o600,
    )?;
    println!("PASS executable CPU/MEM/root-DISK vs independent readings: {evidence}");
    println!("Evidence: {}", base.display());
    Ok(evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_install::release::fixture::Fixture;
    use serde_json::Value;

    const GENERATION: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// An observed profile whose system-status installation is exactly the
    /// synthetic catalogue artifact: the receipt, the manifest and the executable
    /// all match the release catalogue.
    struct Profile {
        fixture: Fixture,
    }

    impl Profile {
        fn new() -> Result<Self> {
            let fixture = Fixture::new()?;
            let package = fixture.root.join(format!(
                "managed/generations/1431055733/system-status/{GENERATION}"
            ));
            fs::create_dir_all(package.join("bin"))?;
            for name in ["among-plugin.json", "SOURCE.json", "bin/plugin"] {
                fs::copy(
                    fixture.root.join("system-status").join(name),
                    package.join(name),
                )?;
            }
            let prepared = fixture.inputs["plugins"]
                .as_array()
                .and_then(|plugins| plugins.iter().find(|p| p["id"] == "system-status"))
                .ok_or("fixture lacks system-status")?;
            let receipt = json!({"repository_id":1431055733u64,"plugin_id":"system-status",
                "generation":GENERATION,"manifest_sha256":prepared["manifest_sha256"],
                "package_sha256":prepared["sha256"]});
            let profile = Self { fixture };
            profile.write_installed(&json!({"installations":[receipt]}))?;
            Ok(profile)
        }

        fn root(&self) -> &Path {
            &self.fixture.root
        }

        fn executable(&self) -> PathBuf {
            self.root().join(format!(
                "managed/generations/1431055733/system-status/{GENERATION}/bin/plugin"
            ))
        }

        fn installed(&self) -> Result<Value> {
            storage::parse_json(&fs::read(self.root().join("managed/installed.json"))?)
        }

        /// Writes the selection and records it in the observation, as the
        /// real observation does.
        fn write_installed(&self, document: &Value) -> Result<()> {
            self.write_unobserved(document)?;
            self.observe()
        }

        fn write_unobserved(&self, document: &Value) -> Result<()> {
            fs::write(
                self.root().join("managed/installed.json"),
                serde_json::to_vec(document)?,
            )?;
            Ok(())
        }

        fn observe(&self) -> Result<()> {
            let selection = fs::read(self.root().join("managed/installed.json"))?;
            fs::write(
                self.root().join("profile.json"),
                serde_json::to_vec(&json!({
                    "kind":"observed_published_installation_profile",
                    "source_revision":self.fixture.inputs["source_revision"],
                    "catalogue_revision":self.fixture.inputs["catalogue_revision"],
                    "architecture":self.fixture.inputs["architecture"],
                    "installed_size":selection.len(),
                    "installed_sha256":storage::sha256(selection.as_slice())?,
                }))?,
            )?;
            Ok(())
        }

        /// Replaces the catalogue the profile names and re-seals its size and
        /// digest, so that only the catalogue's content differs.
        fn change_catalogue(&mut self, change: impl FnOnce(&mut Value)) -> Result<()> {
            change(&mut self.fixture.catalogue);
            let bytes = serde_json::to_vec(&self.fixture.catalogue)?;
            fs::write(self.root().join("among-plugins.json"), &bytes)?;
            self.fixture.inputs["catalogue_size"] = bytes.len().into();
            self.fixture.inputs["catalogue_sha256"] = storage::sha256(bytes.as_slice())?.into();
            for asset in self.fixture.inputs["release_snapshot"]["assets"]
                .as_array_mut()
                .into_iter()
                .flatten()
                .filter(|asset| asset["name"] == "among-plugins.json")
            {
                asset["size"] = bytes.len().into();
                asset["sha256"] = storage::sha256(bytes.as_slice())?.into();
            }
            self.fixture.save()
        }

        fn system_status_version(catalogue: &mut Value) -> Result<&mut Value> {
            catalogue["plugins"]
                .as_array_mut()
                .and_then(|plugins| plugins.iter_mut().find(|p| p["id"] == "system-status"))
                .map(|plugin| &mut plugin["versions"][0])
                .ok_or_else(|| "fixture lacks system-status".into())
        }

        fn change_receipt(&self, key: &str, value: Value) -> Result<()> {
            let mut document = self.installed()?;
            document["installations"][0][key] = value;
            self.write_installed(&document)
        }
    }

    #[test]
    fn the_exact_catalogue_artifact_is_pinned() -> Result<()> {
        let profile = Profile::new()?;
        let pinned = pinned_executable(profile.root())?;
        assert_eq!(pinned.canonicalize()?, profile.executable().canonicalize()?);
        Ok(())
    }

    #[test]
    fn a_manifest_that_agrees_with_the_receipt_but_not_the_catalogue_is_refused() -> Result<()> {
        let profile = Profile::new()?;
        let package = profile
            .executable()
            .parent()
            .and_then(Path::parent)
            .ok_or("package directory absent")?
            .to_path_buf();
        // A different manifest with the same entrypoint, recorded consistently
        // in the receipt: only the catalogue can tell it is foreign.
        let mut manifest: Value =
            storage::parse_json(&fs::read(package.join("among-plugin.json"))?)?;
        manifest["description"] = "not the reviewed manifest".into();
        let bytes = serde_json::to_vec(&manifest)?;
        fs::write(package.join("among-plugin.json"), &bytes)?;
        profile.change_receipt("manifest_sha256", json!(storage::sha256(bytes.as_slice())?))?;
        let error = pinned_executable(profile.root())
            .err()
            .ok_or("a manifest foreign to the catalogue was accepted")?;
        assert!(
            error.to_string().contains("differs from the catalogue"),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn a_catalogue_member_that_is_not_executable_refuses_the_pin() -> Result<()> {
        let mut profile = Profile::new()?;
        profile.change_catalogue(|catalogue| {
            if let Ok(version) = Profile::system_status_version(catalogue) {
                for artifact in version["artifacts"].as_array_mut().into_iter().flatten() {
                    for member in artifact["members"].as_array_mut().into_iter().flatten() {
                        if member["path"] == "bin/plugin" {
                            member["executable"] = false.into();
                        }
                    }
                }
            }
        })?;
        let error = pinned_executable(profile.root())
            .err()
            .ok_or("an executable the catalogue calls plain data was accepted")?;
        assert!(
            error
                .to_string()
                .contains("differs from the catalogue artifact"),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn a_profile_without_its_observation_cannot_be_pinned() -> Result<()> {
        // No observation entry at all.
        let profile = Profile::new()?;
        fs::remove_file(profile.root().join("profile.json"))?;
        assert!(pinned_executable(profile.root()).is_err());

        // An observation of another kind, release or architecture.
        for (key, value) in [
            ("kind", json!("something_else")),
            ("source_revision", json!("b".repeat(40))),
            ("catalogue_revision", json!("c".repeat(40))),
            ("architecture", json!("riscv64")),
        ] {
            let profile = Profile::new()?;
            let mut observation: Value =
                storage::parse_json(&fs::read(profile.root().join("profile.json"))?)?;
            observation[key] = value;
            fs::write(
                profile.root().join("profile.json"),
                serde_json::to_vec(&observation)?,
            )?;
            let error = pinned_executable(profile.root())
                .err()
                .ok_or_else(|| format!("an observation with a foreign {key} was accepted"))?;
            assert!(
                error.to_string().contains("observation differs"),
                "{key}: {error}"
            );
        }

        // A selection edited after it was observed.
        let profile = Profile::new()?;
        let mut selection = profile.installed()?;
        selection["note"] = "edited after the observation".into();
        profile.write_unobserved(&selection)?;
        let error = pinned_executable(profile.root())
            .err()
            .ok_or("a selection changed after its observation was accepted")?;
        assert!(
            error
                .to_string()
                .contains("differs from the observed profile"),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn the_hello_echo_may_narrow_the_granted_features_but_not_the_required_ones() {
        let accepted = |features: Value| {
            let mut reply = hello();
            reply["features"] = features;
            hello_reply_accepted(&reply)
        };
        // The host answers with what the plugin uses out of what was granted.
        assert!(accepted(json!(["status", "system.metrics"])));
        assert!(accepted(json!(["status", "system.metrics", "settings"])));
        assert!(accepted(json!(GRANTED_FEATURES)));
        // A required feature dropped, or one that was never granted.
        assert!(!accepted(json!(["status"])));
        assert!(!accepted(json!(["system.metrics", "settings"])));
        assert!(!accepted(json!(["status", "system.metrics", "network"])));
        assert!(!accepted(json!("status")));
        // The binding and identity must come back unchanged.
        for key in ["identity", "session", "generation", "type"] {
            let mut reply = hello();
            reply["features"] = json!(["status", "system.metrics"]);
            reply[key] = "other".into();
            assert!(!hello_reply_accepted(&reply), "{key}");
        }
    }

    #[test]
    fn a_receipt_naming_another_package_or_manifest_is_refused() -> Result<()> {
        for key in ["package_sha256", "manifest_sha256"] {
            let profile = Profile::new()?;
            profile.change_receipt(key, json!("0".repeat(64)))?;
            let error = pinned_executable(profile.root())
                .err()
                .ok_or_else(|| format!("a foreign {key} was accepted"))?;
            assert!(
                error
                    .to_string()
                    .contains("differs from its selected receipt"),
                "{key}: {error}"
            );
        }
        Ok(())
    }

    #[test]
    fn an_executable_with_other_bytes_or_size_is_refused() -> Result<()> {
        // Same length, different bytes: only the digest can tell.
        // Different length: the recorded size already differs.
        for replacement in [
            b"synthetic non-executable paylOad".to_vec(),
            b"shorter".to_vec(),
        ] {
            let profile = Profile::new()?;
            let path = profile.executable();
            let original = fs::read(&path)?;
            assert_ne!(replacement, original);
            fs::write(&path, &replacement)?;
            let error = pinned_executable(profile.root())
                .err()
                .ok_or("a changed executable was accepted")?;
            assert!(
                error
                    .to_string()
                    .contains("differs from the catalogue artifact"),
                "{error}"
            );
        }
        Ok(())
    }

    #[test]
    fn an_executable_without_the_execute_bit_is_refused() -> Result<()> {
        let profile = Profile::new()?;
        fs::set_permissions(profile.executable(), fs::Permissions::from_mode(0o600))?;
        assert!(pinned_executable(profile.root()).is_err());
        Ok(())
    }

    #[test]
    fn a_missing_or_duplicated_selection_is_refused() -> Result<()> {
        let profile = Profile::new()?;
        let receipt = profile.installed()?["installations"][0].clone();
        profile.write_installed(&json!({"installations":[]}))?;
        assert!(pinned_executable(profile.root())
            .err()
            .ok_or("an absent selection was accepted")?
            .to_string()
            .contains("absent or duplicated"));
        profile.write_installed(&json!({"installations":[receipt.clone(), receipt]}))?;
        assert!(pinned_executable(profile.root())
            .err()
            .ok_or("a duplicated selection was accepted")?
            .to_string()
            .contains("absent or duplicated"));
        // Another plugin's receipt is not the system-status selection.
        let mut other = profile.installed()?["installations"][0].clone();
        other["plugin_id"] = "clock".into();
        profile.write_installed(&json!({"installations":[other]}))?;
        assert!(pinned_executable(profile.root()).is_err());
        Ok(())
    }

    #[test]
    fn a_generation_that_is_not_sixty_four_hex_characters_is_refused() -> Result<()> {
        for generation in [
            "abc".to_string(),
            "g".repeat(64),
            format!("{GENERATION}0"),
            "../".repeat(21) + "x",
        ] {
            let profile = Profile::new()?;
            profile.change_receipt("generation", json!(generation))?;
            let error = pinned_executable(profile.root())
                .err()
                .ok_or_else(|| format!("generation {generation:?} was accepted"))?;
            assert!(
                error
                    .to_string()
                    .contains("invalid system-status generation"),
                "{generation:?}: {error}"
            );
        }
        Ok(())
    }
}
