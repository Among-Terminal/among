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
    let mut output = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        match message["type"].as_str() {
            Some("hello") => send(&mut output, &hello())?,
            Some("status") if answer_status => send(
                &mut output,
                &json!({
                    "type": "status",
                    "request_id": message["request_id"],
                    "fragments": [{"text": "ok", "style": "success", "priority": 10}],
                }),
            )?,
            _ => {}
        }
    }
    Ok(())
}

fn main() -> std::io::Result<()> {
    let mode = std::env::args().nth(1).unwrap_or_default();
    match mode.as_str() {
        "echo" => serve(true),
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
