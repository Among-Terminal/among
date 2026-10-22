use std::io::{self, Write};

use serde::{Deserialize, Serialize};

use super::{Configuration, Outcome, MAX_PAYLOAD};
use crate::notifications::{Event, Kind};

pub(super) const MAX_CONTROL: usize = 128 * 1024;
pub(super) const MAX_REPORT: usize = 512;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub config: Configuration,
    pub payload: Payload,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Payload {
    schema: u8,
    kind: PayloadKind,
    event: Identity,
    source: Option<Target>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<Producer>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PayloadKind {
    TerminalBell,
    Application,
    ManagedNotification,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Producer {
    identity: String,
    generation: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    session: u64,
    id: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    session: u64,
    generation: u64,
    pane: u64,
}

impl Payload {
    pub fn from_event(
        event: Event,
        delivery: Option<&crate::plugin_notifications::DeliveryAuthorization>,
    ) -> io::Result<Self> {
        let origin = match event.kind() {
            Kind::ManagedNotification => {
                let delivery = delivery
                    .filter(|delivery| delivery.job().event() == event)
                    .ok_or_else(|| io::Error::other("missing managed delivery authority"))?;
                let owner = delivery
                    .envelope()
                    .origin
                    .as_ref()
                    .filter(|_| event.origin().is_some())
                    .ok_or_else(|| io::Error::other("missing managed origin"))?;
                Some(Producer {
                    identity: owner.identity.clone(),
                    generation: owner.generation.clone(),
                })
            }
            Kind::TerminalBell | Kind::Application => {
                if event.origin().is_some()
                    || delivery.is_some_and(|delivery| delivery.envelope().origin.is_some())
                {
                    return Err(io::Error::other("host event origin mismatch"));
                }
                None
            }
        };
        Ok(Self {
            schema: 1,
            kind: match event.kind() {
                Kind::TerminalBell => PayloadKind::TerminalBell,
                Kind::Application => PayloadKind::Application,
                Kind::ManagedNotification => PayloadKind::ManagedNotification,
            },
            event: Identity {
                session: event.session(),
                id: event.id(),
            },
            source: event.source().map(|source| Target {
                session: source.session(),
                generation: source.generation(),
                pane: source.pane(),
            }),
            origin,
        })
    }

    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let mut writer = Bounded {
            bytes: Vec::new(),
            maximum: MAX_PAYLOAD,
        };
        serde_json::to_writer(&mut writer, self).map_err(io::Error::other)?;
        writer.write_all(b"\n")?;
        Ok(writer.bytes)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Report {
    pub outcome: Outcome,
    pub output_bytes: usize,
    pub reaped: bool,
}

pub(super) fn encode(request: &Request) -> io::Result<Vec<u8>> {
    let mut writer = Bounded {
        bytes: Vec::new(),
        maximum: MAX_CONTROL,
    };
    serde_json::to_writer(&mut writer, request).map_err(io::Error::other)?;
    let mut framed = (writer.bytes.len() as u32).to_le_bytes().to_vec();
    framed.extend(writer.bytes);
    Ok(framed)
}

struct Bounded {
    bytes: Vec<u8>,
    maximum: usize,
}
impl Write for Bounded {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("notification bound"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn payload_writer_enforces_the_real_sixteen_kib_boundary() {
        let mut writer = Bounded {
            bytes: Vec::new(),
            maximum: MAX_PAYLOAD,
        };
        writer.write_all(&vec![b'a'; MAX_PAYLOAD]).unwrap();
        assert!(writer.write_all(b"x").is_err());
        assert_eq!(writer.bytes.len(), MAX_PAYLOAD);
    }
}
