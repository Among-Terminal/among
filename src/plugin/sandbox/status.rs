//! the child's report to the host. two fixed-size records go through a pipe
//! that closes on `exec`: the first names the process, the second says which
//! protections it applied. both are built without allocation.

/// Bytes of one record. Well under `PIPE_BUF`, so each write is atomic.
pub const RECORD_BYTES: usize = 16;
const VERSION: u8 = 1;

/// Which mandatory step failed, when one did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Step {
    None = 0,
    NoNewPrivs = 1,
    Landlock = 2,
    Seccomp = 3,
    Inheritance = 4,
}

impl Step {
    fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::None,
            1 => Self::NoNewPrivs,
            2 => Self::Landlock,
            3 => Self::Seccomp,
            4 => Self::Inheritance,
            _ => return None,
        })
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::None => "no step failed",
            Self::NoNewPrivs => "setting no-new-privileges failed",
            Self::Landlock => "applying the Landlock filesystem rules failed",
            Self::Seccomp => "loading the seccomp filter failed",
            Self::Inheritance => "closing inherited descriptors failed",
        }
    }
}

/// How completely the child's Landlock ruleset was enforced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Enforcement {
    /// No Landlock restriction is in force.
    None = 0,
    /// Enforced with fewer access rights than requested.
    Partial = 1,
    Full = 2,
}

impl Enforcement {
    fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::None,
            1 => Self::Partial,
            2 => Self::Full,
            _ => return None,
        })
    }
}

/// What the child reports as the final result of its confinement steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub no_new_privs: bool,
    pub seccomp_loaded: bool,
    /// Landlock ABI the child's ruleset was applied with; 0 when none.
    pub landlock_abi: u8,
    pub enforcement: Enforcement,
    pub failed: Step,
    pub errno: u32,
}

impl Report {
    pub fn to_bytes(self) -> [u8; RECORD_BYTES] {
        let mut bytes = [0u8; RECORD_BYTES];
        bytes[0] = VERSION;
        bytes[1] = KIND_FINAL;
        bytes[2] = u8::from(self.no_new_privs) | (u8::from(self.seccomp_loaded) << 1);
        bytes[3] = self.landlock_abi;
        bytes[4] = self.enforcement as u8;
        bytes[5] = self.failed as u8;
        bytes[8..12].copy_from_slice(&self.errno.to_le_bytes());
        bytes
    }
}

const KIND_BEGIN: u8 = 1;
const KIND_FINAL: u8 = 2;

/// The first record: the child's process ID, so the host can kill a start that
/// never reaches `exec` before `Command::spawn` returns.
pub fn begin_record(pid: u32) -> [u8; RECORD_BYTES] {
    let mut bytes = [0u8; RECORD_BYTES];
    bytes[0] = VERSION;
    bytes[1] = KIND_BEGIN;
    bytes[12..16].copy_from_slice(&pid.to_le_bytes());
    bytes
}

/// Why a record could not be accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    /// The stream ended before a whole record arrived.
    Short,
    UnknownVersion,
    UnexpectedKind,
    Malformed,
}

impl Invalid {
    pub fn description(self) -> &'static str {
        match self {
            Self::Short => "the confinement report was incomplete",
            Self::UnknownVersion => "the confinement report has an unknown format version",
            Self::UnexpectedKind => "the confinement report has an unexpected record type",
            Self::Malformed => "the confinement report is malformed",
        }
    }
}

/// Reads the process ID out of a first record.
pub fn parse_begin(bytes: &[u8]) -> Result<u32, Invalid> {
    let record = header(bytes, KIND_BEGIN)?;
    let pid = u32::from_le_bytes(record[12..16].try_into().expect("four bytes"));
    if pid == 0 || record[2..12].iter().any(|byte| *byte != 0) {
        return Err(Invalid::Malformed);
    }
    Ok(pid)
}

/// Reads the final record.
pub fn parse_final(bytes: &[u8]) -> Result<Report, Invalid> {
    let record = header(bytes, KIND_FINAL)?;
    let enforcement = Enforcement::from_byte(record[4]).ok_or(Invalid::Malformed)?;
    let failed = Step::from_byte(record[5]).ok_or(Invalid::Malformed)?;
    if record[2] & !0b11 != 0 || record[6..8] != [0, 0] || record[12..16] != [0; 4] {
        return Err(Invalid::Malformed);
    }
    Ok(Report {
        no_new_privs: record[2] & 1 != 0,
        seccomp_loaded: record[2] & 2 != 0,
        landlock_abi: record[3],
        enforcement,
        failed,
        errno: u32::from_le_bytes(record[8..12].try_into().expect("four bytes")),
    })
}

fn header(bytes: &[u8], kind: u8) -> Result<&[u8], Invalid> {
    let record = bytes.get(..RECORD_BYTES).ok_or(Invalid::Short)?;
    if record[0] != VERSION {
        return Err(Invalid::UnknownVersion);
    }
    if record[1] != kind {
        return Err(Invalid::UnexpectedKind);
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete() -> Report {
        Report {
            no_new_privs: true,
            seccomp_loaded: true,
            landlock_abi: 4,
            enforcement: Enforcement::Full,
            failed: Step::None,
            errno: 0,
        }
    }

    #[test]
    fn a_complete_record_round_trips_every_field() {
        let report = Report {
            errno: 13,
            failed: Step::Seccomp,
            enforcement: Enforcement::Partial,
            landlock_abi: 2,
            seccomp_loaded: false,
            no_new_privs: true,
        };
        assert_eq!(parse_final(&report.to_bytes()), Ok(report));
        assert_eq!(parse_final(&complete().to_bytes()), Ok(complete()));
    }

    #[test]
    fn a_short_record_is_refused_rather_than_zero_filled() {
        let bytes = complete().to_bytes();
        for length in 0..RECORD_BYTES {
            assert_eq!(parse_final(&bytes[..length]), Err(Invalid::Short));
        }
        assert_eq!(parse_begin(&begin_record(7)[..15]), Err(Invalid::Short));
    }

    #[test]
    fn an_unknown_version_is_refused() {
        let mut bytes = complete().to_bytes();
        bytes[0] = 2;
        assert_eq!(parse_final(&bytes), Err(Invalid::UnknownVersion));
        let mut begin = begin_record(7);
        begin[0] = 0;
        assert_eq!(parse_begin(&begin), Err(Invalid::UnknownVersion));
    }

    #[test]
    fn records_of_the_wrong_kind_and_out_of_range_fields_are_refused() {
        assert_eq!(parse_final(&begin_record(7)), Err(Invalid::UnexpectedKind));
        assert_eq!(
            parse_begin(&complete().to_bytes()),
            Err(Invalid::UnexpectedKind)
        );
        for (index, value) in [(4usize, 3u8), (5, 9), (2, 0b100), (6, 1), (12, 1)] {
            let mut bytes = complete().to_bytes();
            bytes[index] = value;
            assert_eq!(parse_final(&bytes), Err(Invalid::Malformed), "byte {index}");
        }
        assert_eq!(parse_begin(&begin_record(0)), Err(Invalid::Malformed));
    }

    #[test]
    fn the_begin_record_carries_the_process_id() {
        assert_eq!(parse_begin(&begin_record(4242)), Ok(4242));
    }
}
