//! bound bytes before parsing: limiting parsed fragments cannot contain a pipe
//! that never sends a newline or a producer faster than the GTK polling loop.

use std::sync::mpsc::{SyncSender, TrySendError};

pub const MAX_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_QUEUE_FRAMES: usize = 16;
pub const MAX_JSON_DEPTH: usize = 32;
pub const MAX_POLL_FRAMES: usize = 256;

#[derive(Default)]
pub struct FrameDecoder {
    partial: Vec<u8>,
}

impl FrameDecoder {
    pub fn push(&mut self, bytes: &[u8], sender: &SyncSender<String>) -> Result<(), &'static str> {
        for part in bytes.split_inclusive(|byte| *byte == b'\n') {
            if self.partial.len() + part.len() > MAX_FRAME_BYTES {
                return Err("plugin stdout frame exceeds 64 KiB or lacks a newline");
            }
            self.partial.extend_from_slice(part);
            if self.partial.last() != Some(&b'\n') {
                if self.partial.len() == MAX_FRAME_BYTES {
                    return Err("plugin stdout frame exceeds 64 KiB or lacks a newline");
                }
                continue;
            }
            self.partial.pop();
            let line = String::from_utf8(std::mem::take(&mut self.partial))
                .map_err(|_| "plugin stdout is not UTF-8")?
                .into_boxed_str()
                .into_string();
            sender.try_send(line).map_err(|err| match err {
                TrySendError::Full(_) => "plugin stdout queue exceeds 16 frames",
                TrySendError::Disconnected(_) => "plugin stdout receiver closed",
            })?;
        }
        Ok(())
    }
}

pub fn depth_is_bounded(bytes: &[u8]) -> bool {
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > MAX_JSON_DEPTH {
                        return false;
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    #[test]
    fn unterminated_input_stops_at_the_frame_budget() {
        let (tx, rx) = sync_channel(MAX_QUEUE_FRAMES);
        let mut decoder = FrameDecoder::default();
        assert!(decoder.push(&vec![b'x'; MAX_FRAME_BYTES * 4], &tx).is_err());
        assert!(decoder.partial.len() <= MAX_FRAME_BYTES);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn exact_frame_boundary_is_accepted_and_queue_overflow_is_finite() {
        let (tx, rx) = sync_channel(MAX_QUEUE_FRAMES);
        let mut frame = vec![b'x'; MAX_FRAME_BYTES - 1];
        frame.push(b'\n');
        let mut decoder = FrameDecoder::default();
        decoder.push(&frame[..1000], &tx).unwrap();
        decoder.push(&frame[1000..], &tx).unwrap();
        assert_eq!(rx.try_recv().unwrap().len(), MAX_FRAME_BYTES - 1);

        let (tx, rx) = sync_channel(MAX_QUEUE_FRAMES);
        let input = "{}\n".repeat(MAX_QUEUE_FRAMES + 1);
        assert_eq!(
            decoder.push(input.as_bytes(), &tx),
            Err("plugin stdout queue exceeds 16 frames")
        );
        assert_eq!(rx.try_iter().count(), MAX_QUEUE_FRAMES);
    }

    #[test]
    fn nesting_ignores_brackets_and_escaped_quotes_inside_strings() {
        assert!(depth_is_bounded(br#"{"text":"[[[\\\"{{{"}"#));
        assert!(depth_is_bounded("[".repeat(MAX_JSON_DEPTH).as_bytes()));
        assert!(!depth_is_bounded("[".repeat(MAX_JSON_DEPTH + 1).as_bytes()));
    }
}
