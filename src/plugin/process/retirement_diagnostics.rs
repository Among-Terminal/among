//! test-only observations, not retirement proof. preserve raw state and errors
//! without introducing a process-support requirement in application builds.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::process::ExitStatus;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use serde_json::{json, Value};

const MAX_EVENTS: usize = 32;
const MAX_STAT_BYTES: u64 = 4096;

pub(super) struct Trace {
    origin: Instant,
    entries: Mutex<Entries>,
}

pub(super) struct Observation {
    pid: u32,
    read_started_us: u128,
    read_finished_us: u128,
    stat: io::Result<String>,
}

#[derive(Default)]
struct Entries {
    events: Vec<Value>,
    dropped: usize,
}

impl Trace {
    pub(super) fn new() -> Self {
        Self {
            origin: Instant::now(),
            entries: Mutex::new(Entries::default()),
        }
    }

    fn entries(&self) -> MutexGuard<'_, Entries> {
        match self.entries.lock() {
            Ok(entries) => entries,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub(super) fn event(&self, name: &str, detail: Value) {
        let mut entries = self.entries();
        if entries.events.len() == MAX_EVENTS {
            entries.dropped = entries.dropped.saturating_add(1);
            return;
        }
        let sequence = entries.events.len();
        entries.events.push(json!({
            "sequence": sequence,
            "at_us": self.origin.elapsed().as_micros(),
            "event": name,
            "detail": detail,
        }));
    }

    pub(super) fn signal(&self, event: &str, target: i32, signal: i32, result: i32) {
        // read errno before any other syscall can overwrite it.
        let error = (result < 0).then(io::Error::last_os_error);
        self.event(
            event,
            json!({
                "target": target, "signal": signal, "result": result,
                "error": error.as_ref().map(error_record),
            }),
        );
    }

    pub(super) fn wait_result(&self, result: &io::Result<ExitStatus>) {
        self.event(
            "leader-wait-returned",
            match result {
                Ok(status) => json!({"status": status.to_string(), "success": status.success()}),
                Err(error) => json!({"error": error_record(error)}),
            },
        );
    }

    pub(super) fn observe(&self, event: &str, pid: u32) -> io::Result<String> {
        self.record_observation(event, self.sample(pid))
    }

    pub(super) fn timestamp_us(&self) -> u128 {
        self.origin.elapsed().as_micros()
    }

    pub(super) fn sample(&self, pid: u32) -> Observation {
        let read_started_us = self.timestamp_us();
        let stat = read_stat(pid);
        let read_finished_us = self.timestamp_us();
        Observation {
            pid,
            read_started_us,
            read_finished_us,
            stat,
        }
    }

    pub(super) fn record_observation(
        &self,
        event: &str,
        sample: Observation,
    ) -> io::Result<String> {
        self.event(
            event,
            json!({
                "read_started_us": sample.read_started_us,
                "read_finished_us": sample.read_finished_us,
                "observation": match &sample.stat {
                    Ok(raw) => stat_record(sample.pid, raw),
                    Err(error) => json!({"pid": sample.pid, "error": error_record(error)}),
                },
            }),
        );
        sample.stat
    }

    pub(super) fn retain(&self, mode: &str) -> (String, io::Result<()>) {
        let entries = self.entries();
        let report = json!({
            "mode": mode,
            "clock": "microseconds since trace creation, monotonic",
            "observer": "unchanged: Z/ENOENT/ESRCH are not live; other raw states are live",
            "caveat": "test instrumentation can perturb scheduling; no retirement proof is added",
            "max_events": MAX_EVENTS,
            "dropped_events": entries.dropped,
            "events": entries.events,
        });
        let text = report.to_string();
        // a failure must carry its own report even when artifact setup fails.
        let retention = retain_report(mode, text.as_bytes());
        let retained = match &retention {
            Ok(Some(path)) => format!("retained={}", path.display()),
            Ok(None) => "AMONG_CHECK_ROOT absent; inline report only".into(),
            Err(error) => format!("diagnostic retention failed: {error}"),
        };
        (
            format!("{retained}; retirement_trace={text}"),
            retention.map(|_| ()),
        )
    }
}

fn read_stat(pid: u32) -> io::Result<String> {
    let mut raw = String::new();
    File::open(format!("/proc/{pid}/stat"))?
        .take(MAX_STAT_BYTES + 1)
        .read_to_string(&mut raw)?;
    if raw.len() as u64 > MAX_STAT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "proc stat exceeded diagnostic bound",
        ));
    }
    Ok(raw)
}

fn error_record(error: &io::Error) -> Value {
    json!({"errno": error.raw_os_error(), "kind": format!("{:?}", error.kind()), "message": error.to_string()})
}

fn stat_record(pid: u32, raw: &str) -> Value {
    // comm can contain spaces and ')'; only the required leading fields are
    // decoded. a malformed observation stays raw instead of becoming 'gone'.
    let fields: Vec<_> = raw
        .rsplit_once(')')
        .map(|(_, tail)| tail.split_whitespace().collect())
        .unwrap_or_default();
    let number = |index: usize| {
        fields
            .get(index)
            .and_then(|value: &&str| value.parse::<u64>().ok())
    };
    json!({
        "pid": pid,
        "stat_pid": raw.split_once('(').and_then(|(head, _)| head.trim().parse::<u32>().ok()),
        "raw_stat": raw,
        "raw_state": fields.first(),
        "ppid": number(1),
        "pgid": number(2),
        "sid": number(3),
        "birth_start_time_ticks": number(19),
    })
}

fn retain_report(mode: &str, bytes: &[u8]) -> io::Result<Option<std::path::PathBuf>> {
    let Some(root) = std::env::var_os("AMONG_CHECK_ROOT") else {
        return Ok(None);
    };
    let root = std::path::PathBuf::from(root);
    if !root.is_absolute() || !root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "AMONG_CHECK_ROOT must be an existing absolute directory",
        ));
    }
    let directory = root.join("plugin-retirement-diagnostics");
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{mode}-{}.json", glib::uuid_string_random()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(Some(path))
}

#[test]
fn stat_fields_keep_birth_identity_and_dead_state_without_reclassification() {
    let raw = format!(
        "123 (fixture ) with spaces) X 10 123 7 {}98765 0\n",
        "0 ".repeat(15)
    );
    let record = stat_record(123, &raw);
    assert_eq!(record["raw_stat"], raw);
    assert_eq!(record["stat_pid"], 123);
    assert_eq!(record["raw_state"], "X");
    assert_eq!(record["pgid"], 123);
    assert_eq!(record["sid"], 7);
    assert_eq!(record["birth_start_time_ticks"], 98765);
    assert!(stat_record(123, "malformed")["raw_state"].is_null());
    assert_eq!(
        error_record(&io::Error::from_raw_os_error(libc::EACCES))["errno"],
        libc::EACCES
    );
}

#[test]
fn excessive_events_are_counted_not_retained_without_bound() {
    let trace = Trace::new();
    for _ in 0..MAX_EVENTS + 3 {
        trace.event("sample", Value::Null);
    }
    let entries = trace.entries();
    assert_eq!(entries.events.len(), MAX_EVENTS);
    assert_eq!(entries.dropped, 3);
}
