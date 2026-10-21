//! a cancelled worker retains its identity slot until it has actually stopped.
//! window replacement cannot start another install against the retiring worker.

use crate::plugin::{data::Problem, manifest::ManagedIdentity};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Weak};

type Key = (PathBuf, String);
static ACTIVE: LazyLock<Mutex<BTreeMap<Key, Weak<Ticket>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

pub(super) struct Ticket {
    pub identity: String,
    pub generation: String,
}

impl Ticket {
    pub fn acquire(root: &Path, identity: ManagedIdentity) -> Result<Arc<Self>, Problem> {
        if !root.is_absolute()
            || root
                .components()
                .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
        {
            return Err(Problem(
                "operation store root must be a plain absolute path",
            ));
        }
        let key = (root.to_path_buf(), identity.key());
        let mut active = ACTIVE
            .lock()
            .map_err(|_| Problem("installer operation authority is unavailable"))?;
        active.retain(|_, ticket| ticket.strong_count() > 0);
        if active.contains_key(&key) {
            return Err(Problem("this plugin identity still has an active or cancelling installation; wait for its worker to stop"));
        }
        let ticket = Arc::new(Self {
            identity: key.1.clone(),
            generation: super::store::token(),
        });
        active.insert(key, Arc::downgrade(&ticket));
        Ok(ticket)
    }
}
