// A managed subprocess fixture whose workspace summaries change on demand.
//
// Every workspace summary it reports is the current content of `summary.txt`
// in its home directory, so a check can change what the sidebar should show at
// a moment of its choosing by rewriting that file. Other scopes get no
// fragments. The summary is re-collected as often as the host allows.
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

fn summary() -> String {
    std::env::var_os("HOME")
        .map(|home| std::path::PathBuf::from(home).join("summary.txt"))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|text| text.trim().to_string())
        .unwrap_or_default()
}

fn main() -> io::Result<()> {
    let mut output = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let line = line?;
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match message["type"].as_str() {
            Some("shutdown") => break,
            Some("managed_hello") => writeln!(output, "{message}")?,
            Some("status") => {
                let fragments = if message["scope"]["slot"] == "workspace_summary" {
                    json!([{ "text": summary() }])
                } else {
                    json!([])
                };
                writeln!(
                    output,
                    "{}",
                    json!({"type":"status","session":message["session"],
                        "generation":message["generation"],"request_id":message["request_id"],
                        "refresh_ms":0,"fragments":fragments})
                )?;
            }
            _ => {}
        }
        output.flush()?;
    }
    Ok(())
}
