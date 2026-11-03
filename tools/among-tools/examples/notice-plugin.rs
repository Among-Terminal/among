// managed runtime fixture: unsupported notify cannot become UI or a fake event.
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

fn main() -> io::Result<()> {
    let mut output = io::stdout().lock();
    let mut pane_opened = false;
    for line in io::stdin().lock().lines() {
        let message: Value = serde_json::from_str(&line?)?;
        match message["type"].as_str() {
            Some("shutdown") => break,
            Some("managed_hello") => serde_json::to_writer(&mut output, &message)?,
            Some("event") => {
                if message["event"]["kind"] == "pane_opened" {
                    pane_opened = true;
                }
                continue;
            }
            Some("status") => {
                writeln!(
                    output,
                    "{{\"type\":\"notify\",\"message\":\"notification-check-must-not-escape\"}}"
                )?;
                let fragments = if message["scope"]["slot"] == "app_footer" {
                    vec![
                        json!({"text":if pane_opened {"fixture-status-pane-opened"}else{"fixture-status"}}),
                    ]
                } else {
                    vec![]
                };
                serde_json::to_writer(
                    &mut output,
                    &json!({"type":"status","session":message["session"],"generation":message["generation"],"request_id":message["request_id"],"refresh_ms":1000,"fragments":fragments}),
                )?;
            }
            _ => continue,
        };
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}
