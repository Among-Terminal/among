//! Session files contain layout data, never commands or process state.

use crate::tree::{Node, Orientation, PaneId};
use crate::workspace::{PaneAllocator, Workspace, WorkspaceSet, PALETTE_SIZE};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
const MAX_PANES: usize = 4096;
const MAX_WORKSPACES: usize = 256;
static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    pub active: usize,
    pub workspaces: Vec<SavedWorkspace>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedWorkspace {
    pub name: String,
    pub colour: usize,
    pub focused: u64,
    pub layout: SavedNode,
    pub directories: Vec<SavedDirectory>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedDirectory {
    pub pane: u64,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SavedNode {
    Pane {
        id: u64,
    },
    Split {
        orientation: SavedOrientation,
        ratio: f64,
        first: Box<SavedNode>,
        second: Box<SavedNode>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedOrientation {
    Vertical,
    Horizontal,
}

impl SavedNode {
    fn capture(node: &Node, depth: usize) -> Result<Self, String> {
        if depth > MAX_DEPTH {
            return Err("session layout exceeds the depth limit".into());
        }
        Ok(match node {
            Node::Pane { id } => Self::Pane { id: id.0 },
            Node::Split {
                orientation,
                ratio,
                first,
                second,
            } => Self::Split {
                orientation: match orientation {
                    Orientation::Vertical => SavedOrientation::Vertical,
                    Orientation::Horizontal => SavedOrientation::Horizontal,
                },
                ratio: *ratio,
                first: Box::new(Self::capture(first, depth + 1)?),
                second: Box::new(Self::capture(second, depth + 1)?),
            },
        })
    }

    fn validate(&self, depth: usize, ids: &mut BTreeSet<u64>) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("session layout exceeds the depth limit".into());
        }
        match self {
            Self::Pane { id } => {
                if *id == 0 || !ids.insert(*id) {
                    return Err("pane identifiers must be nonzero and unique".into());
                }
                if ids.len() > MAX_PANES {
                    return Err("session exceeds the pane limit".into());
                }
            }
            Self::Split {
                ratio,
                first,
                second,
                ..
            } => {
                if !ratio.is_finite() || !(0.05..=0.95).contains(ratio) {
                    return Err("split ratio must be finite and between 0.05 and 0.95".into());
                }
                first.validate(depth + 1, ids)?;
                second.validate(depth + 1, ids)?;
            }
        }
        Ok(())
    }

    fn restore(&self) -> Node {
        match self {
            Self::Pane { id } => Node::Pane { id: PaneId(*id) },
            Self::Split {
                orientation,
                ratio,
                first,
                second,
            } => Node::Split {
                orientation: match orientation {
                    SavedOrientation::Vertical => Orientation::Vertical,
                    SavedOrientation::Horizontal => Orientation::Horizontal,
                },
                ratio: *ratio,
                first: Box::new(first.restore()),
                second: Box::new(second.restore()),
            },
        }
    }
}

impl Snapshot {
    pub fn capture(
        set: &WorkspaceSet,
        cwd: impl Fn(PaneId) -> Option<String>,
    ) -> Result<Self, String> {
        if set.len() > MAX_WORKSPACES {
            return Err("session exceeds the workspace limit".into());
        }
        let mut workspaces = Vec::new();
        for workspace in set.workspaces() {
            workspaces.push(SavedWorkspace {
                name: workspace.name.clone(),
                colour: workspace.colour,
                focused: workspace.focused.0,
                layout: SavedNode::capture(&workspace.nodes, 0)?,
                directories: workspace
                    .panes()
                    .into_iter()
                    .map(|pane| SavedDirectory {
                        pane: pane.0,
                        cwd: cwd(pane),
                    })
                    .collect(),
            });
        }
        let snapshot = Self {
            version: 1,
            active: set.active_index(),
            workspaces,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("session file exceeds 8 MiB".into());
        }
        // serde_json's recursion guard stays enabled: hostile nesting is rejected
        // before typed recursive trees can consume an unbounded stack.
        let snapshot: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err(format!("unsupported session version {}", self.version));
        }
        if self.workspaces.is_empty()
            || self.workspaces.len() > MAX_WORKSPACES
            || self.active >= self.workspaces.len()
        {
            return Err("invalid workspace count or active workspace".into());
        }
        let mut all_ids = BTreeSet::new();
        for workspace in &self.workspaces {
            if workspace.name.trim().is_empty()
                || workspace.name.len() > 4096
                || workspace.name.trim() != workspace.name
                || workspace.colour >= PALETTE_SIZE
            {
                return Err("invalid workspace name or colour".into());
            }
            let mut ids = BTreeSet::new();
            workspace.layout.validate(0, &mut ids)?;
            if !ids.contains(&workspace.focused) {
                return Err("focused pane is not in its workspace".into());
            }
            if workspace.directories.len() != ids.len() {
                return Err("directory entries must match layout leaves".into());
            }
            let mut directories = BTreeSet::new();
            for entry in &workspace.directories {
                if !ids.contains(&entry.pane) || !directories.insert(entry.pane) {
                    return Err("invalid or duplicate directory pane".into());
                }
                if let Some(cwd) = &entry.cwd {
                    if cwd.len() > 65536 || cwd.contains('\0') || !Path::new(cwd).is_absolute() {
                        return Err("working directory must be an absolute path without NUL".into());
                    }
                }
            }
            for id in ids {
                if !all_ids.insert(id) {
                    return Err("pane identifier reused across workspaces".into());
                }
                if all_ids.len() > MAX_PANES {
                    return Err("session exceeds the pane limit".into());
                }
            }
        }
        Ok(())
    }

    pub fn restore(&self) -> Result<(WorkspaceSet, PaneAllocator), String> {
        self.validate()?;
        let mut last = 0;
        let workspaces = self
            .workspaces
            .iter()
            .map(|saved| {
                let nodes = saved.layout.restore();
                for pane in nodes.panes() {
                    last = last.max(pane.0);
                }
                Workspace {
                    reference: saved.focused,
                    name: saved.name.clone(),
                    colour: saved.colour,
                    focused: PaneId(saved.focused),
                    running: nodes.len(),
                    nodes,
                    zoomed: None,
                    cwd: saved
                        .directories
                        .iter()
                        .map(|entry| (PaneId(entry.pane), entry.cwd.clone()))
                        .collect(),
                }
            })
            .collect();
        let set = WorkspaceSet::from_restored(workspaces, self.active)
            .ok_or("invalid restored workspace set")?;
        Ok((set, PaneAllocator::after(PaneId(last))))
    }
}

pub fn path() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .map(|base| base.join("among/session.json"))
}

pub fn initial_directory(requested: Option<&str>) -> (Option<String>, bool) {
    fn accessible(path: &str) -> bool {
        let Ok(path_c) = std::ffi::CString::new(path) else {
            return false;
        };
        Path::new(path).is_absolute() && Path::new(path).is_dir()
            // Check effective-user search permission; mode bits alone miss ACLs.
            && unsafe { libc::faccessat(libc::AT_FDCWD, path_c.as_ptr(), libc::X_OK, libc::AT_EACCESS) } == 0
    }
    if let Some(path) = requested.filter(|path| accessible(path)) {
        return (Some(path.into()), false);
    }
    if requested.is_none() {
        if let Some(path) = std::env::current_dir()
            .ok()
            .and_then(|path| path.to_str().map(str::to_owned))
            .filter(|path| accessible(path))
        {
            return (Some(path), false);
        }
    }
    let directory = std::env::var("HOME")
        .ok()
        .filter(|path| accessible(path))
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|path| path.to_str().map(str::to_owned))
                .filter(|path| accessible(path))
        });
    (directory, requested.is_some())
}

struct Job {
    snapshot: Snapshot,
    final_write: bool,
}
#[derive(Default)]
struct Queue {
    pending: Option<Job>,
    stopped: bool,
}

pub struct Completion {
    pub snapshot: Snapshot,
    pub result: Result<(), String>,
    pub final_write: bool,
}

/// One writer owns disk ordering; pending captures coalesce instead of piling up
/// while fsync is slow. No GTK value crosses the worker boundary.
pub struct Writer {
    queue: std::sync::Arc<(std::sync::Mutex<Queue>, std::sync::Condvar)>,
    completed: std::sync::mpsc::Receiver<Completion>,
}

impl Writer {
    pub fn new(path: PathBuf) -> Result<Self, String> {
        Self::with_save(path, save)
    }

    pub(crate) fn with_save(
        path: PathBuf,
        mut persist: impl FnMut(&Path, &Snapshot) -> Result<(), String> + Send + 'static,
    ) -> Result<Self, String> {
        let queue = std::sync::Arc::new((
            std::sync::Mutex::new(Queue::default()),
            std::sync::Condvar::new(),
        ));
        let worker = queue.clone();
        let (sender, completed) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("among-session-writer".into())
            .spawn(move || loop {
                let (lock, ready) = &*worker;
                let mut queue = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                while queue.pending.is_none() && !queue.stopped {
                    queue = ready
                        .wait(queue)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
                let Some(job) = queue.pending.take() else {
                    break;
                };
                drop(queue);
                let result = persist(&path, &job.snapshot);
                if sender
                    .send(Completion {
                        snapshot: job.snapshot,
                        result,
                        final_write: job.final_write,
                    })
                    .is_err()
                    || job.final_write
                {
                    break;
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self { queue, completed })
    }

    pub fn submit(&self, snapshot: Snapshot, final_write: bool) {
        let (lock, ready) = &*self.queue;
        let mut queue = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if !queue.stopped {
            queue.pending = Some(Job {
                snapshot,
                final_write,
            });
            queue.stopped = final_write;
            ready.notify_one();
        }
    }

    pub fn drain(&self) -> Vec<Completion> {
        self.completed.try_iter().collect()
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let (lock, ready) = &*self.queue;
        let mut queue = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        queue.stopped = true;
        ready.notify_one();
    }
}

fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.file_type().is_file() {
        return Err("refusing non-regular session file".into());
    }
    let mut bytes = Vec::new();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| error.to_string())?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .file_type()
        .is_file()
    {
        return Err("refusing non-regular session file".into());
    }
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("session file exceeds 8 MiB".into());
    }
    Ok(Some(bytes))
}

pub fn load(path: &Path) -> Result<Option<Snapshot>, String> {
    read(path)?.map(|bytes| Snapshot::parse(&bytes)).transpose()
}

pub fn save(path: &Path, snapshot: &Snapshot) -> Result<(), String> {
    save_with_hooks(path, snapshot, |_| Ok(()), |_| Ok(()))
}

fn save_with_hooks(
    path: &Path,
    snapshot: &Snapshot,
    before_rename: impl FnOnce(&Path) -> Result<(), String>,
    sync_parent: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    snapshot.validate()?;
    let original = read(path)?;
    if let Some(bytes) = &original {
        Snapshot::parse(bytes)?;
    }
    let bytes = serde_json::to_vec_pretty(snapshot).map_err(|error| error.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("session file exceeds 8 MiB".into());
    }
    let parent = path.parent().ok_or("session file has no parent")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mode = std::fs::symlink_metadata(path)
        .map(|m| m.permissions().mode() & 0o600)
        .unwrap_or(0o600);
    let temporary = parent.join(format!(
        ".among-session-{}-{}",
        std::process::id(),
        NEXT_FILE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    let prepared = (|| {
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        before_rename(&temporary)?;
        if read(path)? != original {
            return Err("session file changed concurrently; retry checkpoint".into());
        }
        std::fs::rename(&temporary, path).map_err(|error| error.to_string())
    })();
    if prepared.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    prepared?;
    std::fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|error| format!("session written, durability uncertain: {error}"))?;
    sync_parent(parent).map_err(|error| format!("session written, durability uncertain: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Snapshot {
        Snapshot::parse(br#"{"version":1,"active":0,"workspaces":[{"name":"project","colour":0,"focused":1,"layout":{"type":"pane","id":1},"directories":[{"pane":1,"cwd":null}]}]}"#).unwrap()
    }

    #[test]
    fn documented_snapshot_loads_without_touching_preferences() {
        let docs = include_str!("../docs/sessions.md");
        let json = docs
            .split("```json\n")
            .nth(1)
            .unwrap()
            .split("\n```")
            .next()
            .unwrap();
        let temp = Temp::new();
        let path = temp.0.join("session.json");
        let preferences = temp.0.join("plugins.json");
        std::fs::write(&preferences, "personal preferences").unwrap();
        std::fs::write(&path, json).unwrap();
        let snapshot = load(&path).unwrap().unwrap();
        save(&path, &snapshot).unwrap();
        assert_eq!(snapshot, fixture());
        assert_eq!(
            std::fs::read_to_string(preferences).unwrap(),
            "personal preferences"
        );
    }

    #[test]
    fn nested_unicode_round_trip_and_sparse_ids() {
        let mut ws = Workspace::new("项目 one".into(), PaneId(7), 3);
        ws.split_focused(800, 400, PaneId(1_000_000_000_000))
            .unwrap();
        ws.split_focused(200, 600, PaneId(12)).unwrap();
        ws.nodes.set_ratio_at(&[], 0.3);
        ws.nodes.set_ratio_at(&[1], 0.7);
        let mut set = WorkspaceSet::new(ws);
        set.push_active(Workspace::new("second".into(), PaneId(23), 6));
        let snapshot = Snapshot::capture(&set, |id| {
            (id.0 != 12).then(|| format!("/synthetic/目录/{}", id.0))
        })
        .unwrap();
        let encoded = serde_json::to_vec(&snapshot).unwrap();
        let decoded = Snapshot::parse(&encoded).unwrap();
        assert_eq!(snapshot, decoded);
        let (restored, mut allocator) = decoded.restore().unwrap();
        assert_eq!(restored.active_index(), 1);
        for (a, b) in restored.workspaces().iter().zip(set.workspaces()) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.colour, b.colour);
            assert_eq!(a.nodes, b.nodes);
            assert_eq!(a.focused, b.focused);
        }
        assert_eq!(allocator.try_next(), Some(PaneId(1_000_000_000_001)));
        assert_eq!(restored.workspaces()[0].cwd_of(PaneId(12)), None);
    }

    #[test]
    fn reordered_version_one_snapshots_keep_order_active_identity_and_pane_associations() {
        let mut first = Workspace::new("one".into(), PaneId(1), 0);
        first.split_focused(800, 300, PaneId(2)).unwrap();
        first.nodes.set_ratio_at(&[], 0.3);
        let mut set = WorkspaceSet::new(first);
        set.push_active(Workspace::new("two".into(), PaneId(10), 1));
        set.push_active(Workspace::new("three".into(), PaneId(20), 2));
        set.activate(0);
        for (source, gap, expected) in [
            (2, 0, ["three", "one", "two"]),
            (0, 3, ["one", "two", "three"]),
        ] {
            set.reorder(source, gap).unwrap();
            let snapshot =
                Snapshot::capture(&set, |id| Some(format!("/workspace/pane-{}", id.0))).unwrap();
            assert_eq!(snapshot.version, 1);
            let (restored, _) = Snapshot::parse(&serde_json::to_vec(&snapshot).unwrap())
                .unwrap()
                .restore()
                .unwrap();
            assert_eq!(
                restored
                    .workspaces()
                    .iter()
                    .map(|workspace| workspace.name.as_str())
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(restored.active().name, "one");
            assert_eq!(restored.active_index(), set.active_index());
            for (before, after) in set.workspaces().iter().zip(restored.workspaces()) {
                assert_eq!(before.nodes, after.nodes);
                assert_eq!(before.colour, after.colour);
                assert_eq!(before.focused, after.focused);
                for id in before.panes() {
                    assert_eq!(
                        after.cwd_of(id),
                        Some(format!("/workspace/pane-{}", id.0).as_str())
                    );
                }
            }
        }
    }

    #[test]
    fn a_pane_moved_between_workspaces_round_trips_with_its_directory() {
        let mut first = Workspace::new("one".into(), PaneId(1), 0);
        first.split_focused(800, 300, PaneId(2)).unwrap();
        first.set_cwd(PaneId(2), Some("/moved/directory".into()));
        let mut set = WorkspaceSet::new(first);
        set.push_active(Workspace::new("two".into(), PaneId(10), 1));
        let source = set.workspaces()[0].reference;
        let target = set.workspaces()[1].reference;

        let movement = set
            .move_pane_between(source, target, PaneId(2), Orientation::Vertical)
            .expect("the pane moves");
        assert!(!movement.source_closed);

        let snapshot = Snapshot::capture(&set, |id| {
            set.workspaces()
                .iter()
                .find(|workspace| workspace.panes().contains(&id))
                .and_then(|workspace| workspace.cwd_of(id).map(str::to_owned))
        })
        .expect("the moved session is capturable");
        assert_eq!(snapshot.version, 1);
        assert_eq!(snapshot.active, 1);
        assert_eq!(
            snapshot.workspaces[0].layout.restore().panes(),
            vec![PaneId(1)]
        );
        assert_eq!(
            snapshot.workspaces[1].layout.restore().panes(),
            vec![PaneId(10), PaneId(2)]
        );
        assert_eq!(snapshot.workspaces[1].focused, 2);

        let (restored, _) = Snapshot::parse(&serde_json::to_vec(&snapshot).unwrap())
            .unwrap()
            .restore()
            .unwrap();
        assert_eq!(restored.active_index(), 1);
        assert_eq!(restored.active().name, "two");
        assert_eq!(restored.active().focused, PaneId(2));
        assert_eq!(
            restored.workspaces()[1].cwd_of(PaneId(2)),
            Some("/moved/directory")
        );
        assert_eq!(restored.workspaces()[0].panes(), vec![PaneId(1)]);
    }

    #[test]
    fn a_move_that_empties_the_source_does_not_restore_an_empty_workspace() {
        let mut first = Workspace::new("one".into(), PaneId(1), 0);
        first.set_cwd(PaneId(1), Some("/from/one".into()));
        let mut set = WorkspaceSet::new(first);
        set.push_active(Workspace::new("two".into(), PaneId(10), 1));
        let source = set.workspaces()[0].reference;
        let target = set.workspaces()[1].reference;

        let movement = set
            .move_pane_between(source, target, PaneId(1), Orientation::Horizontal)
            .expect("the pane moves");
        assert!(movement.source_closed);
        assert_eq!(movement.target, 0);

        let snapshot = Snapshot::capture(&set, |id| {
            set.workspaces()
                .iter()
                .find(|workspace| workspace.panes().contains(&id))
                .and_then(|workspace| workspace.cwd_of(id).map(str::to_owned))
        })
        .expect("the moved session is capturable");
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(snapshot.workspaces[0].name, "two");
        assert_eq!(snapshot.workspaces[0].focused, 1);
        assert_eq!(
            snapshot.workspaces[0].layout.restore().panes(),
            vec![PaneId(10), PaneId(1)]
        );

        let (restored, _) = Snapshot::parse(&serde_json::to_vec(&snapshot).unwrap())
            .unwrap()
            .restore()
            .unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored.active().name, "two");
        assert_eq!(restored.active().focused, PaneId(1));
        assert_eq!(
            restored.workspaces()[0].cwd_of(PaneId(1)),
            Some("/from/one")
        );
        assert!(restored
            .workspaces()
            .iter()
            .all(|workspace| workspace.name != "one"));
    }

    #[test]
    fn exhausted_identifiers_never_overflow_or_repeat() {
        let mut allocator = PaneAllocator::after(PaneId(u64::MAX - 1));
        assert_eq!(allocator.try_next(), Some(PaneId(u64::MAX)));
        assert_eq!(allocator.try_next(), None);
        assert_eq!(allocator.try_next(), None);
    }

    #[test]
    fn rejects_invalid_documents_without_reconstruction() {
        let baseline = serde_json::to_value(fixture()).unwrap();
        for (pointer, value) in [
            ("/version", json!(2)),
            ("/active", json!(1)),
            ("/workspaces/0/name", json!(" ")),
            ("/workspaces/0/colour", json!(8)),
            ("/workspaces/0/focused", json!(2)),
            ("/workspaces/0/layout/id", json!(0)),
            ("/workspaces/0/directories/0/pane", json!(2)),
            ("/workspaces/0/directories/0/cwd", json!("relative")),
            ("/workspaces/0/directories/0/cwd", json!("/bad\0path")),
            ("/workspaces/0/directories", json!([])),
        ] {
            let mut invalid = baseline.clone();
            *invalid.pointer_mut(pointer).unwrap() = value;
            assert!(
                Snapshot::parse(&serde_json::to_vec(&invalid).unwrap()).is_err(),
                "{pointer}"
            );
        }
        let mut invalid = fixture();
        invalid.workspaces.push(invalid.workspaces[0].clone());
        assert!(invalid.restore().is_err());
        let mut value = baseline;
        value["commands"] = json!(["dangerous command"]);
        assert!(Snapshot::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn limits_depth_counts_size_and_ratios() {
        let mut snapshot = fixture();
        for ratio in [f64::NAN, f64::INFINITY, 0.0, 1.0] {
            snapshot.workspaces[0].layout = SavedNode::Split {
                orientation: SavedOrientation::Vertical,
                ratio,
                first: Box::new(SavedNode::Pane { id: 1 }),
                second: Box::new(SavedNode::Pane { id: 2 }),
            };
            assert!(snapshot.validate().is_err());
        }
        let mut node = SavedNode::Pane { id: 1 };
        for id in 2..=67 {
            node = SavedNode::Split {
                orientation: SavedOrientation::Horizontal,
                ratio: 0.5,
                first: Box::new(node),
                second: Box::new(SavedNode::Pane { id }),
            };
        }
        assert!(node.validate(0, &mut BTreeSet::new()).is_err());
        fn balanced(start: u64, count: u64) -> SavedNode {
            if count == 1 {
                return SavedNode::Pane { id: start };
            }
            let half = count / 2;
            SavedNode::Split {
                orientation: SavedOrientation::Vertical,
                ratio: 0.5,
                first: Box::new(balanced(start, half)),
                second: Box::new(balanced(start + half, count - half)),
            }
        }
        assert!(balanced(1, MAX_PANES as u64 + 1)
            .validate(0, &mut BTreeSet::new())
            .is_err());
        let mut snapshot = fixture();
        snapshot.workspaces = vec![snapshot.workspaces[0].clone(); MAX_WORKSPACES + 1];
        assert!(snapshot.validate().is_err());
        assert!(Snapshot::parse(&vec![b' '; MAX_BYTES + 1]).is_err());
        let excessive = format!("{}0{}", "[".repeat(1000), "]".repeat(1000));
        assert!(Snapshot::parse(excessive.as_bytes()).is_err());
    }

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            // Disposable and removed on drop; nothing here is evidence.
            let path = std::env::temp_dir().join(format!(
                "among-session-unit-{}-{}",
                std::process::id(),
                NEXT_FILE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn storage_is_atomic_private_and_preserves_unsafe_documents() {
        let temp = Temp::new();
        let path = temp.0.join("session.json");
        assert_eq!(load(&path).unwrap(), None);
        save(&path, &fixture()).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let original = std::fs::read(&path).unwrap();
        let mut changed = fixture();
        changed.workspaces[0].name = "changed".into();
        assert!(
            save_with_hooks(&path, &changed, |_| Err("interrupted".into()), |_| Ok(())).is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(std::fs::read_dir(&temp.0).unwrap().count(), 1);
        for bytes in [b"malformed".as_slice(), br#"{"version":99}"#.as_slice()] {
            std::fs::write(&path, bytes).unwrap();
            assert!(load(&path).is_err());
            assert!(save(&path, &changed).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        std::fs::remove_file(&path).unwrap();
        let target = temp.0.join("target");
        std::fs::write(&target, &original).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(save(&path, &changed).is_err());
        assert!(load(&path).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), original);
    }

    #[test]
    fn concurrent_changes_and_uncertain_durability_are_reported() {
        let temp = Temp::new();
        let path = temp.0.join("session.json");
        save(&path, &fixture()).unwrap();
        let mut changed = fixture();
        changed.workspaces[0].name = "changed".into();
        let mut external = fixture();
        external.workspaces[0].name = "external".into();
        let error = save_with_hooks(
            &path,
            &changed,
            |_| {
                std::fs::write(&path, serde_json::to_vec(&external).unwrap()).unwrap();
                Ok(())
            },
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(error.contains("concurrently"));
        assert_eq!(load(&path).unwrap(), Some(external));
        let error = save_with_hooks(&path, &changed, |_| Ok(()), |_| Err("sync failure".into()))
            .unwrap_err();
        assert!(error.contains("durability uncertain"));
        assert_eq!(load(&path).unwrap(), Some(changed));
    }

    #[test]
    fn permissions_failures_recover_without_damaging_snapshot() {
        let temp = Temp::new();
        let path = temp.0.join("session.json");
        save(&path, &fixture()).unwrap();
        let original = std::fs::read(&path).unwrap();
        std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(save(&path, &fixture()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::set_permissions(&temp.0, std::fs::Permissions::from_mode(0o700)).unwrap();
        save(&path, &fixture()).unwrap();
    }

    #[test]
    fn slow_writer_coalesces_and_final_state_cannot_be_overwritten() {
        let temp = Temp::new();
        let path = temp.0.join("session.json");
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut first = true;
        let writer = Writer::with_save(path.clone(), move |path, snapshot| {
            if first {
                first = false;
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
            save(path, snapshot)
        })
        .unwrap();
        writer.submit(fixture(), false);
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let started = std::time::Instant::now();
        for index in 0..100 {
            let mut snapshot = fixture();
            snapshot.workspaces[0].name = format!("pending {index}");
            writer.submit(snapshot, false);
        }
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        assert!(writer.queue.0.lock().unwrap().pending.is_some());
        let mut final_state = fixture();
        final_state.workspaces[0].name = "final".into();
        writer.submit(final_state.clone(), true);
        writer.submit(fixture(), false); // submissions after finalization are refused
        release_tx.send(()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut completed = Vec::new();
        while std::time::Instant::now() < deadline {
            completed.extend(writer.drain());
            if completed.iter().any(|completion| completion.final_write) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            completed.len(),
            2,
            "one in-flight plus latest pending snapshot"
        );
        assert!(completed.iter().all(|completion| completion.result.is_ok()));
        assert!(completed.last().unwrap().final_write);
        assert_eq!(load(&path).unwrap(), Some(final_state));
    }
}
