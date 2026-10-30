//! unshipped synthetic managed process: tests control actual stdout frames via
//! owned package-local input files, and retain both sides of the wire.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;

fn send(output: &mut impl Write, sent: &mut impl Write, value: &Value) -> std::io::Result<()> {
    writeln!(sent, "{value}")?;
    sent.flush()?;
    writeln!(output, "{value}")?;
    output.flush()
}

fn substitute(value: &mut Value, hello: &Value) {
    match value {
        Value::String(text) if text == "$session" => *value = hello["session"].clone(),
        Value::String(text) if text == "$generation" => *value = hello["generation"].clone(),
        Value::Array(values) => values.iter_mut().for_each(|value| substitute(value, hello)),
        Value::Object(values) => values
            .values_mut()
            .for_each(|value| substitute(value, hello)),
        _ => {}
    }
}

fn write_owned(path: &str, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes.as_ref())
}

fn main() -> std::io::Result<()> {
    let mut input = std::io::stdin();
    let flags = unsafe { libc::fcntl(input.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(input.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let open = |name| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(name)
    };
    let mut received = open("received.jsonl")?;
    let mut sent = open("sent.jsonl")?;
    let mut output = std::io::stdout().lock();
    let mut hello = Value::Null;
    let mut buffer = Vec::new();
    let mut command = std::fs::read_to_string("next-command")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1);
    let mut held = Vec::new();
    write_owned("executed", std::process::id().to_string())?;
    loop {
        let mut bytes = [0; 4096];
        match input.read(&mut bytes) {
            Ok(0) => return Ok(()),
            Ok(count) => buffer.extend_from_slice(&bytes[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        if buffer.len() > 128 * 1024 {
            return Err(std::io::Error::other("fixture input overflow"));
        }
        while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
            let message: Value = serde_json::from_slice(&buffer[..newline])?;
            buffer.drain(..=newline);
            writeln!(received, "{message}")?;
            received.flush()?;
            match message["type"].as_str() {
                Some("managed_hello") => {
                    hello = message.clone();
                    if let Ok(bytes) = std::fs::read("pre-admission.json") {
                        let mut premature: Value = serde_json::from_slice(&bytes)?;
                        substitute(&mut premature, &hello);
                        send(&mut output, &mut sent, &premature)?;
                    }
                    send(&mut output, &mut sent, &message)?;
                }
                Some("configure") => send(
                    &mut output,
                    &mut sent,
                    &json!({"type":"configured","session":message["session"],"generation":message["generation"],"revision":message["revision"],"outcome":"applied"}),
                )?,
                Some("status") => {
                    if !std::path::Path::new("hold-status").exists() {
                        send(
                            &mut output,
                            &mut sent,
                            &json!({"type":"status","session":message["session"],"generation":message["generation"],"request_id":message["request_id"],"fragments":[{"text":"responsive"}]}),
                        )?;
                    }
                }
                Some("deliver") => {
                    let mode = std::fs::read_to_string("delivery-mode")
                        .unwrap_or_else(|_| "accepted".into());
                    let outcome = match mode.as_str() {
                        "refused" => "refused",
                        "failed" => "failed",
                        "uncertain" => "uncertain",
                        _ => "accepted",
                    };
                    let result = json!({"type":"delivery_result","session":message["session"],"generation":message["generation"],"job_id":message["delivery"]["job_id"],"outcome":outcome});
                    match mode.as_str() {
                        "stall" | "late" if held.len() < 15 => held.push(result),
                        "double" => {
                            send(&mut output, &mut sent, &result)?;
                            send(&mut output, &mut sent, &result)?;
                        }
                        "cycle" => {
                            send(&mut output, &mut sent, &result)?;
                            send(
                                &mut output,
                                &mut sent,
                                &json!({"type":"notification_event","session":message["session"],"generation":message["generation"],"event":{"sequence":1000,"target":{"kind":"application"},"message":"echo","causal_job":message["delivery"]["job_id"]}}),
                            )?;
                        }
                        "flood" => {
                            for _ in 0..4096 {
                                send(&mut output, &mut sent, &result)?;
                            }
                        }
                        _ => send(&mut output, &mut sent, &result)?,
                    }
                }
                Some("shutdown") => return Ok(()),
                _ => {}
            }
        }
        if !hello.is_null() {
            let path = format!("input-{command}.json");
            if let Ok(bytes) = std::fs::read(&path) {
                let mut message: Value = serde_json::from_slice(&bytes)?;
                substitute(&mut message, &hello);
                send(&mut output, &mut sent, &message)?;
                std::fs::remove_file(path)?;
                command += 1;
                write_owned("next-command", command.to_string())?;
            }
            if std::path::Path::new("release-deliveries").exists() {
                for result in held.drain(..) {
                    send(&mut output, &mut sent, &result)?;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
