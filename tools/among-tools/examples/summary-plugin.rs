// A subprocess fixture whose workspace summaries change on demand.
//
// Every workspace summary it reports is the current content of `summary.txt`
// next to its own executable, so a check can change what the sidebar should
// show at a moment of its choosing by rewriting that file. Other scopes get no
// fragments. The summary is re-collected as often as the host allows.
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

fn summary() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("summary.txt")))
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
            Some("hello") => {
                writeln!(output, "{}", json!({"type":"hello","major":1,"minor":1}))?;
            }
            Some("status") => {
                let Some(id) = message["request_id"].as_u64() else {
                    continue;
                };
                let scope = message["scope"].as_str().unwrap_or_default();
                let fragments = if scope.starts_with("workspace_summary:") {
                    json!([{ "text": summary() }])
                } else {
                    json!([])
                };
                writeln!(
                    output,
                    "{}",
                    json!({"type":"status","request_id":id,"refresh_ms":0,"fragments":fragments})
                )?;
            }
            _ => {}
        }
        output.flush()?;
    }
    Ok(())
}
