//! one application budget spans live and retiring windows. a detached worker
//! keeps its reservation; an unrelated identity does not wait for that worker.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Weak};

pub(super) const MAX_PROCESSES: usize = 32;
const REAPING: i32 = -1;
const GROUP_PRESENT: i32 = -2;

pub(super) struct Lease {
    retirement: AtomicI32,
}

impl Lease {
    pub(super) fn new() -> Self {
        Self {
            retirement: AtomicI32::new(0),
        }
    }

    pub(super) fn reaping(&self) {
        self.retirement.store(REAPING, Ordering::Release);
    }

    pub(super) fn pending(&self, result: &std::io::Result<bool>) {
        let value = match result {
            Ok(_) => GROUP_PRESENT,
            Err(error) => error.raw_os_error().unwrap_or(libc::EIO),
        };
        self.retirement.store(value, Ordering::Release);
    }

    pub(super) fn reason(&self) -> Option<&'static str> {
        match self.retirement.load(Ordering::Acquire) {
            0 => None,
            REAPING => Some("waiting for the supervised leader to be reaped"),
            GROUP_PRESENT => Some("owned process-group disappearance is not confirmed; live members or unreaped zombies may remain"),
            libc::EPERM | libc::EACCES => Some("permission denied while checking owned process-group disappearance; generation remains supervised"),
            _ => Some("owned process-group disappearance could not be checked; generation remains supervised"),
        }
    }
}

#[derive(Clone, Default)]
pub struct Admission {
    // Only the GUI thread accesses this map. Workers share the atomic lease,
    // never a lock that could block admission on a slow retirement operation.
    leases: Rc<RefCell<BTreeMap<String, Weak<Lease>>>>,
}

pub(super) enum Busy {
    Identity(&'static str),
    Capacity,
    Borrowed,
}

impl Admission {
    pub(super) fn acquire(&self, identity: &str) -> Result<Arc<Lease>, Busy> {
        let Ok(mut leases) = self.leases.try_borrow_mut() else {
            return Err(Busy::Borrowed);
        };
        leases.retain(|_, lease| lease.strong_count() != 0);
        Self::reserve(&mut leases, identity)
    }

    fn reserve(
        leases: &mut BTreeMap<String, Weak<Lease>>,
        identity: &str,
    ) -> Result<Arc<Lease>, Busy> {
        if let Some(lease) = leases.get(identity).and_then(Weak::upgrade) {
            return Err(Busy::Identity(
                lease
                    .reason()
                    .unwrap_or("previous generation is still supervised"),
            ));
        }
        // A detached worker can drop its last strong reference after retain
        // inspected it. Its now-expired same-key entry must not count as a full
        // budget and permanently fault the replacement instead of admitting it.
        leases.remove(identity);
        if leases.len() >= MAX_PROCESSES {
            return Err(Busy::Capacity);
        }
        let lease = Arc::new(Lease::new());
        leases.insert(identity.into(), Arc::downgrade(&lease));
        Ok(lease)
    }

    pub(super) fn held(&self, identity: &str) -> bool {
        let Ok(leases) = self.leases.try_borrow() else {
            return true;
        };
        leases
            .get(identity)
            .is_some_and(|lease| lease.strong_count() != 0)
    }

    pub(super) fn reason(&self, identity: &str) -> Option<&'static str> {
        let Ok(leases) = self.leases.try_borrow() else {
            return Some("shared plugin admission is busy");
        };
        leases.get(identity).and_then(Weak::upgrade).map(|lease| {
            lease
                .reason()
                .unwrap_or("previous generation is still supervised")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservations_are_identity_scoped_shared_and_bounded() {
        let admission = Admission::default();
        let replacement = admission.clone();
        let mut leases = Vec::new();
        for index in 0..MAX_PROCESSES {
            leases.push(admission.acquire(&format!("p{index}")).ok().unwrap());
        }
        assert!(matches!(replacement.acquire("p0"), Err(Busy::Identity(_))));
        assert!(matches!(
            replacement.acquire("unrelated"),
            Err(Busy::Capacity)
        ));
        drop(leases.pop());
        let unrelated = replacement.acquire("unrelated").ok().unwrap();
        assert!(replacement.held("p0"));
        assert!(!replacement.held("p31"));
        drop(unrelated);
        drop(leases);
        assert!(replacement.acquire("p0").is_ok());
    }

    #[test]
    fn reentrant_admission_defers_without_blocking_or_panicking() {
        let admission = Admission::default();
        let borrowed = admission.leases.borrow_mut();
        assert!(matches!(admission.acquire("probe"), Err(Busy::Borrowed)));
        assert!(
            admission.held("probe"),
            "borrow failure must not produce a stop acknowledgement"
        );
        drop(borrowed);
        assert!(!admission.held("probe"));
        assert!(admission.acquire("probe").is_ok());
    }

    #[test]
    fn worker_expiry_between_prune_and_reserve_cannot_fault_its_own_replacement_as_full() {
        let admission = Admission::default();
        let other: Vec<_> = (0..MAX_PROCESSES - 1)
            .map(|index| admission.acquire(&format!("p{index}")).ok().unwrap())
            .collect();
        let old = admission.acquire("replacement").ok().unwrap();
        let mut snapshot = admission.leases.borrow_mut();
        snapshot.retain(|_, lease| lease.strong_count() != 0);
        // This is the actual map state if the worker's final reference expires
        // after acquire's prune but before its identity lookup/capacity check.
        drop(old);
        assert_eq!(snapshot.len(), MAX_PROCESSES);
        let replacement = Admission::reserve(&mut snapshot, "replacement")
            .ok()
            .unwrap();
        assert_eq!(snapshot.len(), MAX_PROCESSES);
        assert_eq!(other.len() + 1, MAX_PROCESSES);
        drop(replacement);
    }
}
