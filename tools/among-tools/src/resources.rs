use crate::process::{allowed_environment, OwnedProcess};
use crate::{storage, Result};
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
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

pub fn response(bytes: &[u8], id: u64) -> Result<Vec<Value>> {
    let value = storage::parse_json(bytes)?;
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
        json!({"type":"status", "request_id":id, "scope":"app_header", "context":{}})
    )?;
    stdin.flush()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut line = Vec::new();
    loop {
        let mut byte = [0];
        match stdout.read(&mut byte) {
            Ok(0) => return Err("plugin closed output before its response".into()),
            Ok(_) if byte[0] == b'\n' => return response(&line, id),
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

pub fn run(binary: &Path, scratch: &Path) -> Result<Value> {
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
