use super::*;
use std::cell::Cell;
use std::collections::BTreeSet;
use std::io::Cursor;
use std::rc::Rc;

fn reviewer_children() -> BTreeSet<i32> {
    (900_000..900_005).chain(1_000_000..1_009_000).collect()
}

fn records(children: &BTreeSet<i32>) -> Vec<u8> {
    children
        .iter()
        .map(|pid| format!("{pid} "))
        .collect::<String>()
        .into_bytes()
}

#[test]
fn reviewer_9005_mixed_width_children_never_select_an_unframed_pid_prefix() {
    let children = reviewer_children();
    let bytes = records(&children);
    assert_eq!(children.len(), 9005);
    assert_eq!(bytes.len(), 72035);
    let mut selected = Vec::new();
    signal_children(Cursor::new(&bytes), |pid| selected.push(pid));
    let foreign: Vec<_> = selected
        .iter()
        .filter(|pid| !children.contains(pid))
        .collect();
    println!(
        "valid children={}, list bytes={}, selected={}, foreign={foreign:?}; no signals",
        children.len(),
        bytes.len(),
        selected.len()
    );
    assert!(
        foreign.is_empty(),
        "selected a non-child PID prefix: {foreign:?}"
    );
}

fn select(reader: impl Read) -> Vec<i32> {
    let mut selected = Vec::new();
    signal_children(reader, |pid| selected.push(pid));
    selected
}

#[test]
fn byte_cap_before_at_and_after_pid_delimiters_never_invents_a_target() {
    let allowed = BTreeSet::from([1_234_567, 42]);
    for consumed in 0..=11 {
        let mut bytes = vec![b' '; MAX_CHILD_BYTES - consumed];
        bytes.extend_from_slice(b"1234567 42 ");
        let selected = select(Cursor::new(bytes));
        assert!(
            selected.iter().all(|pid| allowed.contains(pid)),
            "offset={consumed}"
        );
        let expected = match consumed {
            0..=7 => vec![],
            8..=10 => vec![1_234_567],
            _ => vec![1_234_567, 42],
        };
        assert_eq!(selected, expected, "offset={consumed}");
    }
}

#[test]
fn exact_cap_eof_requires_a_delimiter_but_observed_short_eof_frames_a_pid() {
    for (suffix, expected) in [
        (b"1234567 ".as_slice(), vec![1_234_567]),
        (b"1234567".as_slice(), vec![]),
        (b"1 ".as_slice(), vec![1]),
        (b"1".as_slice(), vec![]),
        (b" \t".as_slice(), vec![]),
    ] {
        let mut bytes = vec![b' '; MAX_CHILD_BYTES - suffix.len()];
        bytes.extend_from_slice(suffix);
        assert_eq!(select(Cursor::new(bytes)), expected);
    }
    assert_eq!(select(Cursor::new(b"1234567")), vec![1_234_567]);
    assert!(select(Cursor::new(b"")).is_empty());
}

struct Chunks {
    data: Cursor<Vec<u8>>,
    maximum: usize,
    interrupt: bool,
    fail_at: Option<(u64, io::ErrorKind)>,
    bytes: Rc<Cell<usize>>,
    calls: Rc<Cell<usize>>,
}

impl Read for Chunks {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let calls = self.calls.get() + 1;
        self.calls.set(calls);
        if self.interrupt && calls % 2 == 1 {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let mut maximum = buffer.len().min(self.maximum);
        if let Some((offset, kind)) = self.fail_at {
            if self.data.position() >= offset {
                return Err(kind.into());
            }
            maximum = maximum.min((offset - self.data.position()) as usize);
        }
        let count = self.data.read(&mut buffer[..maximum])?;
        self.bytes.set(self.bytes.get() + count);
        Ok(count)
    }
}

fn chunks(bytes: &[u8], maximum: usize, interrupt: bool) -> Chunks {
    Chunks {
        data: Cursor::new(bytes.to_vec()),
        maximum,
        interrupt,
        fail_at: None,
        bytes: Rc::new(Cell::new(0)),
        calls: Rc::new(Cell::new(0)),
    }
}

#[test]
fn partial_reads_and_interruptions_carry_tokens_only_within_one_scan() {
    let bytes = b"7 1234567\t42\n2147483647";
    let allowed = BTreeSet::from([7, 1_234_567, 42, i32::MAX]);
    for maximum in [1, 2, 3, 7, 4096] {
        for interrupt in [false, true] {
            let selected = select(chunks(bytes, maximum, interrupt));
            assert_eq!(selected, vec![7, 1_234_567, 42, i32::MAX]);
            assert!(selected.iter().all(|pid| allowed.contains(pid)));
        }
    }
    let bytes = records(&reviewer_children());
    let reader = chunks(&bytes, 1, false);
    let calls = reader.calls.clone();
    let read = reader.bytes.clone();
    let selected = select(reader);
    let allowed = reviewer_children();
    assert!(selected.iter().all(|pid| allowed.contains(pid)));
    assert_eq!(read.get(), MAX_CHILD_BYTES);
    assert_eq!(calls.get(), MAX_CHILD_BYTES);
}

#[test]
fn invalid_overflow_nonpositive_and_non_ascii_tokens_cannot_signal_a_prefix() {
    let bytes = b"12x34 12\0 -4 +5 0 0000000000 2147483648 9999999999999999999999999 \xff23 23 2147483647 00042";
    let selected = select(chunks(bytes, 1, true));
    let allowed = BTreeSet::from([23, i32::MAX, 42]);
    assert_eq!(selected, vec![23, i32::MAX, 42]);
    assert!(selected.iter().all(|pid| allowed.contains(pid)));
}

#[test]
fn read_errors_and_retry_exhaustion_never_flush_an_unframed_suffix() {
    for kind in [io::ErrorKind::Other, io::ErrorKind::WouldBlock] {
        let mut reader = chunks(b"123 456", 2, true);
        reader.fail_at = Some((7, kind));
        assert_eq!(select(reader), vec![123]);
    }
    let mut reader = chunks(b"123 456", 2, false);
    reader.fail_at = Some((7, io::ErrorKind::Interrupted));
    let calls = reader.calls.clone();
    assert_eq!(select(reader), vec![123]);
    assert_eq!(calls.get(), MAX_CHILD_BYTES);
    // a later poll starts a new list, not the failed scan's numeric suffix.
    assert_eq!(select(Cursor::new(b"789 ")), vec![789]);
}

type ReadCounters = (Rc<Cell<usize>>, Rc<Cell<usize>>);

struct Children {
    original: BTreeSet<i32>,
    current: BTreeSet<i32>,
    killed: BTreeSet<i32>,
    selected: BTreeSet<i32>,
    group_signals: usize,
    reaping: bool,
    reads: Vec<ReadCounters>,
    waits: usize,
}

impl CleanupIo for Children {
    type Children = Chunks;

    fn children(&mut self) -> io::Result<Chunks> {
        self.reaping = false;
        let reader = chunks(&records(&self.current), 4096, false);
        self.reads
            .push((reader.bytes.clone(), reader.calls.clone()));
        Ok(reader)
    }

    fn kill(&mut self, pid: i32) {
        if pid < 0 {
            assert_eq!(self.group_signals, 0, "group signal repeated after reaping");
            assert!(
                self.current.contains(&-pid),
                "leader PID was no longer reserved"
            );
            self.group_signals += 1;
        } else {
            assert!(
                !self.reaping,
                "direct-child signal followed waitpid in the same scan"
            );
            assert!(
                self.original.contains(&pid),
                "selected unrelated synthetic PID {pid}"
            );
            assert!(
                self.current.contains(&pid),
                "selected reaped synthetic PID {pid}"
            );
            self.killed.insert(pid);
            self.selected.insert(pid);
        }
    }

    fn wait(&mut self) -> io::Result<(i32, i32)> {
        self.reaping = true;
        self.waits += 1;
        if let Some(pid) = self.killed.pop_first() {
            assert!(self.current.remove(&pid));
            Ok((pid, 0))
        } else if self.current.is_empty() {
            Err(io::Error::from_raw_os_error(libc::ECHILD))
        } else {
            Ok((0, 0))
        }
    }
}

#[test]
fn cleanup_poll_consumes_framed_selection_and_progresses_through_the_full_9005_tree() {
    let original = reviewer_children();
    let mut system = Children {
        current: original.clone(),
        original: original.clone(),
        killed: BTreeSet::new(),
        selected: BTreeSet::new(),
        group_signals: 0,
        reaping: false,
        reads: Vec::new(),
        waits: 0,
    };
    let mut cleanup = Cleanup {
        leader: 900_000,
        started: Instant::now(),
        killed: false,
        status: None,
    };
    assert!(!cleanup.poll(&mut system));
    assert!(system.reads.is_empty());
    assert_eq!(system.group_signals, 0);
    assert_eq!(system.waits, 0);
    cleanup.started = Instant::now() - CLEANUP_GRACE;
    let mut done = false;
    let mut passes = 0;
    while passes < 40 {
        let previous = system.current.len();
        let waits = system.waits;
        done = cleanup.poll(&mut system);
        assert!(system.waits - waits <= 256);
        assert!(system.current.len() < previous || done);
        passes += 1;
        if done {
            break;
        }
    }
    assert!(
        done,
        "bounded passes did not progress through adopted children"
    );
    assert!(system.current.is_empty());
    assert_eq!(system.selected, original);
    assert_eq!(system.group_signals, 1);
    assert_eq!(cleanup.status.unwrap().code(), Some(0));
    assert!(system
        .reads
        .iter()
        .all(|(bytes, calls)| bytes.get() <= MAX_CHILD_BYTES && calls.get() <= MAX_CHILD_BYTES));
    println!("Cleanup::poll: {passes} bounded passes selected/reaped all 9005 synthetic children, one pre-reap group KILL, no signals sent");
}
