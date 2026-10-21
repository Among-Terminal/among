//! Scripted plugin processes for the plugin host's unit tests.
//!
//! The host tests need real child processes that misbehave in controlled ways:
//! one that answers, one that never answers a status request, one that dies at
//! once and one that writes a line that is not JSON. `cargo test` builds this
//! example next to the test executable, so the tests launch it by path and the
//! host exercises its actual spawn, pipe and fault handling. It is a test
//! fixture, not a shipped plugin.

use serde_json::{json, Value};
use std::io::{BufRead, Write};

fn hello() -> Value {
    json!({"type": "hello", "major": 1, "minor": 0})
}

fn send(output: &mut impl Write, message: &Value) -> std::io::Result<()> {
    writeln!(output, "{message}")?;
    output.flush()
}

/// Reads requests until the host closes stdin. Blank and unparseable lines are
/// ignored, as a tolerant plugin would.
fn serve(answer_status: bool) -> std::io::Result<()> {
    serve_with_delay(answer_status, std::time::Duration::ZERO, None)
}

fn serve_with_delay(
    answer_status: bool,
    delay: std::time::Duration,
    refresh_ms: Option<u64>,
) -> std::io::Result<()> {
    let mut output = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        match message["type"].as_str() {
            Some("hello") => send(&mut output, &hello())?,
            Some("status") if answer_status => {
                std::thread::sleep(delay);
                let mut response = json!({
                    "type": "status",
                    "request_id": message["request_id"],
                    "fragments": [{"text": "ok", "style": "success", "priority": 10}],
                });
                if let Some(interval) = refresh_ms {
                    response["refresh_ms"] = json!(interval);
                }
                send(&mut output, &response)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn write_owned(path: &str, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes.as_ref())
}

fn serve_managed(mode: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if mode == "managed-ignore-term" {
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
        }
    }
    let mut output = std::io::stdout().lock();
    let mut transcript = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open("transcript.jsonl")?;
    // a killed previous generation can leave an incomplete diagnostic record.
    // delimit it so it cannot absorb this generation's actual hello.
    transcript.write_all(b"\n")?;
    write_owned("executed", std::process::id().to_string())?;
    write_owned(
        "credential-inherited",
        if std::env::var_os("AMONG_MANAGED_TEST_CREDENTIAL").is_some() {
            "yes"
        } else {
            "no"
        },
    )?;
    let names: std::collections::BTreeSet<_> = std::env::vars_os()
        .map(|(name, _)| name.to_string_lossy().into_owned())
        .collect();
    write_owned("environment-names.json", serde_json::to_vec(&names)?)?;
    let mut label = "managed".to_string();
    let mut enumerating = false;
    let mut enumeration_id = 1000u64;
    let mut previous_configuration: Option<Value> = None;
    let mut previous_status: Option<Value> = None;
    for line in std::io::stdin().lock().lines() {
        let message: Value = serde_json::from_str(&line?)?;
        let mut record = serde_json::to_vec(&message)?;
        record.push(b'\n');
        transcript.write_all(&record)?;
        transcript.flush()?;
        match message["type"].as_str() {
            Some("managed_hello") => {
                if mode == "managed-hanging" {
                    continue;
                }
                if mode == "managed-before-admission" {
                    send(
                        &mut output,
                        &json!({"type":"status","session":message["session"],"generation":message["generation"],"request_id":1,"fragments":[{"text":"premature"}]}),
                    )?;
                }
                let mut response = message.clone();
                if mode == "managed-wrong-generation" {
                    response["generation"] = json!("old-generation");
                }
                if mode == "managed-wrong-identity" {
                    response["identity"] = json!("github:999/other");
                }
                if mode == "managed-wrong-session" {
                    response["session"] = json!("expired-session");
                }
                if mode == "managed-missing-required" {
                    response["features"] = json!(["status"]);
                }
                if mode == "managed-ungranted-feature" {
                    response["features"] = json!(["status", "settings", "context.title"]);
                }
                if mode == "managed-wrong-version" {
                    response["minor"] = json!(99);
                }
                if mode == "managed-optional-subset" {
                    response["features"] = Value::Array(
                        response["features"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|feature| feature.as_str() != Some("context.title"))
                            .cloned()
                            .collect(),
                    );
                }
                send(&mut output, &response)?;
            }
            Some("configure") => {
                if mode == "managed-held-config"
                    && !std::path::Path::new("allow-configuration").exists()
                {
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(7);
                    while !std::path::Path::new("allow-configuration").exists()
                        && std::time::Instant::now() < deadline
                    {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                }
                if mode == "managed-stale-config" {
                    if let Some(previous) = previous_configuration.take() {
                        send(&mut output, &previous)?;
                        std::fs::write("old-ack-sent", b"yes")?;
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(3);
                        while !std::path::Path::new("allow-current-ack").exists()
                            && std::time::Instant::now() < deadline
                        {
                            std::thread::sleep(std::time::Duration::from_millis(2));
                        }
                    } else {
                        previous_configuration = Some(
                            json!({"type":"configured","session":message["session"],"generation":message["generation"],"revision":message["revision"],"outcome":"applied"}),
                        );
                        continue;
                    }
                }
                if mode == "managed-no-config-reply" {
                    continue;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
                if mode != "managed-reject" {
                    label = message["values"]["label"]
                        .as_str()
                        .unwrap_or("managed")
                        .into();
                }
                let outcome = match mode {
                    "managed-reject" => "rejected",
                    "managed-restart" => "restart_required",
                    _ => "applied",
                };
                send(
                    &mut output,
                    &json!({"type":"configured","session":message["session"],"generation":message["generation"],"revision":message["revision"],"outcome":outcome,"reason":"invalid_configuration","message":message["values"]}),
                )?;
                if let Some(previous) = previous_status.take() {
                    std::fs::write("new-settings-applied", b"yes")?;
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                    while !std::path::Path::new("release-old-status").exists()
                        && std::time::Instant::now() < deadline
                    {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    send(&mut output, &previous)?;
                    std::fs::write("old-status-released", b"yes")?;
                }
                if mode == "managed-reject" {
                    send(
                        &mut output,
                        &json!({"type":"notify","message":message["values"]}),
                    )?;
                }
            }
            Some("status") => {
                if mode == "managed-resource" {
                    label = std::fs::read_to_string("version-resource")?;
                    write_owned("resource-observed", &label)?;
                }
                let held_summary =
                    std::fs::read_to_string("hold-summary")
                        .ok()
                        .is_some_and(|reference| {
                            message["scope"]["slot"] == "workspace_summary"
                                && message["scope"]["target"]["reference"].as_str()
                                    == Some(reference.as_str())
                        });
                if (mode == "managed-held-status" || held_summary)
                    && !std::path::Path::new("allow-status").exists()
                {
                    std::fs::write("status-held", b"yes")?;
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
                    while !std::path::Path::new("allow-status").exists()
                        && std::time::Instant::now() < deadline
                    {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                }
                let target_label = if std::path::Path::new("target-labels").exists()
                    && message["scope"]["slot"] == "workspace_summary"
                {
                    format!(
                        "workspace-{}",
                        message["scope"]["target"]["reference"]
                            .as_str()
                            .unwrap_or_default()
                    )
                } else {
                    label.clone()
                };
                let refresh = if mode.starts_with("managed-on-event") {
                    Value::Null
                } else {
                    json!(1000)
                };
                let response = json!({"type":"status","session":message["session"],"generation":message["generation"],"request_id":message["request_id"],"refresh_ms":refresh,"fragments":[{"text":if held_summary {"retired-target-marker"} else {&target_label}}]});
                if mode == "managed-on-event-held"
                    && message["scope"]["slot"] == "app_footer"
                    && !std::path::Path::new("old-status-held").exists()
                {
                    previous_status = Some(response);
                    std::fs::write("old-status-held", b"yes")?;
                    continue;
                }
                send(&mut output, &response)?;
                if held_summary {
                    std::fs::write("summary-released", b"yes")?;
                    std::thread::sleep(std::time::Duration::from_millis(300));
                }
                if mode == "managed-enumerate"
                    && !enumerating
                    && message["scope"]["slot"] == "app_header"
                    && !message["context"]["next_cursor"].is_null()
                {
                    enumerating = true;
                    send(
                        &mut output,
                        &json!({"type":"enumerate","session":message["session"],"generation":message["generation"],"request_id":enumeration_id,"scope":message["scope"],"cursor":message["context"]["next_cursor"]}),
                    )?;
                }
            }
            Some("context_page") if mode == "managed-enumerate" => {
                if message["context"]["next_cursor"].is_null() {
                    enumerating = false;
                    std::fs::write("enumeration-complete", b"yes")?;
                } else {
                    enumeration_id += 1;
                    send(
                        &mut output,
                        &json!({"type":"enumerate","session":message["session"],"generation":message["generation"],"request_id":enumeration_id,"scope":message["scope"],"cursor":message["context"]["next_cursor"]}),
                    )?;
                }
            }
            Some("shutdown") => return Ok(()),
            _ => {}
        }
    }
    Ok(())
}

fn main() -> std::io::Result<()> {
    // real discovery starts argv-less executables; only this unshipped fixture
    // reads the isolated test's environment to select that behaviour.
    let mode = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("AMONG_FAKE_PLUGIN_MODE").ok())
        .or_else(|| std::fs::read_to_string("among-fake-mode").ok())
        .unwrap_or_default();
    match mode.as_str() {
        mode if mode.starts_with("managed-") => serve_managed(mode),
        "echo" => serve(true),
        "delayed" => serve_with_delay(true, std::time::Duration::from_millis(30), None),
        "rapid" => serve_with_delay(true, std::time::Duration::from_millis(1), Some(0)),
        // Completes the handshake and then never answers a status request, so
        // the host has to time the request out instead of blocking on it.
        "hanging" => serve(false),
        // Greets and exits with a failure status before reading anything.
        "crashing" => {
            send(&mut std::io::stdout().lock(), &hello())?;
            std::process::exit(1);
        }
        // greets and writes bad JSON, then answers valid requests so the host
        // can prove the malformed line did not stop its request/response path.
        "malformed" => {
            let mut output = std::io::stdout().lock();
            send(&mut output, &hello())?;
            writeln!(output, "this is not json")?;
            output.flush()?;
            drop(output);
            serve(true)
        }
        "unterminated" => {
            std::io::stdout()
                .lock()
                .write_all(&vec![b'x'; 256 * 1024])?;
            serve(false)
        }
        "flood" => {
            let mut output = std::io::stdout().lock();
            send(&mut output, &hello())?;
            for _ in 0..4096 {
                send(&mut output, &json!({"type":"notify", "message":"flood"}))?;
            }
            drop(output);
            serve(true)
        }
        "never-read" | "stubborn" | "group-tree" | "inherited" => {
            if mode == "never-read" {
                use std::os::fd::AsRawFd;
                // make a single large request block on every Linux pipe size,
                // without consuming the host's entire frame queue first.
                if unsafe { libc::fcntl(std::io::stdin().as_raw_fd(), libc::F_SETPIPE_SZ, 4096) }
                    < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            if mode != "never-read" {
                // the supervisor must escalate rather than assume TERM worked.
                unsafe {
                    libc::signal(libc::SIGTERM, libc::SIG_IGN);
                }
            }
            let mut output = std::io::stdout().lock();
            send(&mut output, &hello())?;
            if matches!(mode.as_str(), "group-tree" | "inherited") {
                let child = std::process::Command::new(std::env::current_exe()?)
                    .arg("stubborn")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::inherit())
                    .spawn()?;
                send(
                    &mut output,
                    &json!({"type":"notify", "message":child.id().to_string()}),
                )?;
                // retained by the supervised group, not by this fixture's wait.
                drop(child);
            }
            drop(output);
            if mode == "inherited" {
                std::thread::sleep(std::time::Duration::from_millis(300));
                return Ok(());
            }
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        }
        "unmatched" => {
            let mut output = std::io::stdout().lock();
            send(&mut output, &hello())?;
            send(
                &mut output,
                &json!({"type":"status", "request_id":99999, "fragments":[{"text":"unsolicited"}]}),
            )?;
            drop(output);
            serve(true)
        }
        "duplicate" => {
            let mut output = std::io::stdout().lock();
            for line in std::io::stdin().lock().lines() {
                let message: Value = serde_json::from_str(&line?)?;
                if message["type"] == "hello" {
                    send(&mut output, &hello())?;
                }
                if message["type"] == "status" {
                    let response = json!({"type":"status", "request_id":message["request_id"], "fragments":[{"text":"once"}]});
                    send(&mut output, &response)?;
                    send(&mut output, &response)?;
                }
            }
            Ok(())
        }
        "one-status" => {
            let mut output = std::io::stdout().lock();
            for line in std::io::stdin().lock().lines() {
                let message: Value = serde_json::from_str(&line?)?;
                if message["type"] == "hello" {
                    send(&mut output, &hello())?;
                }
                if message["type"] == "status" {
                    send(
                        &mut output,
                        &json!({"type":"status", "request_id":message["request_id"], "fragments":[{"text":"vanishing"}]}),
                    )?;
                }
                if message["type"] == "event" {
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        _ => {
            eprintln!("unknown fake-plugin behaviour");
            std::process::exit(2);
        }
    }
}
