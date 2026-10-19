// A subprocess fixture: notifications and explicit slots are separate messages.
use std::io::{self, BufRead, Write};

fn main() -> io::Result<()> {
    let mut output = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.contains("\"type\":\"shutdown\"") {
            break;
        }
        if line.contains("\"type\":\"hello\"") {
            writeln!(output, "{{\"type\":\"hello\",\"major\":1,\"minor\":1}}")?;
            writeln!(
                output,
                "{{\"type\":\"notify\",\"message\":\"notification-check\"}}"
            )?;
        } else if line.contains("\"kind\":\"pane_opened\"") {
            writeln!(
                output,
                "{{\"type\":\"notify\",\"message\":\"pane-opened-check\"}}"
            )?;
        } else if let Some((_, tail)) = line.split_once("\"request_id\":") {
            let id: String = tail
                .trim_start()
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            writeln!(output, "{{\"type\":\"status\",\"request_id\":{id},\"refresh_ms\":1000,\"fragments\":[{{\"text\":\"fixture-status\"}}]}}")?;
        }
        output.flush()?;
    }
    Ok(())
}
