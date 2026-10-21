//! removal intent invalidates downloads before waiting for supervised stop. only
//! the live host may acknowledge that stop; view lifetime is not authority.

use super::{operations::Ticket, source::Permit, store};
use crate::plugin::{data::Problem, inventory::Receipt};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    RestorePrevious,
    Uninstall,
}

pub struct Job {
    ready: Arc<AtomicBool>,
    result: mpsc::Receiver<Result<store::Commit, Problem>>,
    pub identity: String,
    pub generation: String,
    state: Arc<AtomicU8>,
}

impl Job {
    pub fn start(
        root: PathBuf,
        expected: Receipt,
        action: Action,
        stored: Option<Value>,
    ) -> Result<Self, Problem> {
        let permit = Permit::acquire()?;
        let ticket = match action {
            Action::RestorePrevious => Ticket::acquire(&root, expected.identity()?)?,
            Action::Uninstall => Ticket::removal(&root, expected.identity()?)?,
        };
        let identity = ticket.identity.clone();
        let generation = ticket.generation.clone();
        let ready = Arc::new(AtomicBool::new(action != Action::Uninstall));
        let worker_ready = ready.clone();
        let state = Arc::new(AtomicU8::new(0));
        let worker_state = state.clone();
        let (send, result) = mpsc::sync_channel(1);
        std::thread::Builder::new().name("among-package-mutation".into()).spawn(move || {
            let _permit=permit;let deadline=Instant::now()+Duration::from_secs(60);
            let check=|| {
                if ticket.cancelled.load(Ordering::Acquire){Err(Problem("package operation was invalidated by a later removal"))}
                else if Instant::now()>=deadline{Err(Problem("package operation exceeded its deadline"))}else{Ok(())}
            };
            let outcome=(|| {
                let stop_deadline=Instant::now()+Duration::from_secs(5);
                while !worker_ready.load(Ordering::Acquire) {
                    check()?;
                    if Instant::now()>=stop_deadline{return Err(Problem("supervised stop was not confirmed within five seconds; package remains selected"));}
                    std::thread::sleep(Duration::from_millis(2));
                }
                store::mutate(&root,&expected,action,stored.as_ref(),&check,|| {
                    check()?;worker_state.store(1,Ordering::Release);Ok(())
                })
            })();
            drop(ticket);worker_state.store(2,Ordering::Release);let _=send.send(outcome);
        }).map_err(|_|Problem("cannot start bounded package mutation worker"))?;
        Ok(Self {
            ready,
            result,
            identity,
            generation,
            state,
        })
    }

    pub fn supervised_stopped(&self) {
        self.ready.store(true, Ordering::Release);
    }

    pub fn phase(&self) -> &'static str {
        if !self.ready.load(Ordering::Acquire) {
            "Waiting for supervised plugin stop; runtime authority revoked"
        } else if self.state.load(Ordering::Acquire) == 1 {
            "Committing package selection; cancellation is too late"
        } else if self.state.load(Ordering::Acquire) == 2 {
            "Package operation finished; awaiting result"
        } else {
            "Checking current package selection"
        }
    }

    pub fn poll(&self) -> Option<Result<store::Commit, Problem>> {
        match self.result.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(_) => Some(Err(Problem(
                "package mutation worker stopped without a result",
            ))),
        }
    }
}
