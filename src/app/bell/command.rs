//! lazy filesystem work has one bounded process-wide worker, not one thread per
//! preferences window. neither inspecting nor accepting a configuration runs it.

use among::notifications::{
    commands::{self, OwnedDirectory, Runtime},
    preferences::Preferences,
    Authorization, Channel, Host, Policy,
};
use among::plugin_notifications::DeliveryAuthorization;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{sync_channel, Receiver, SyncSender},
    Arc, OnceLock,
};
use std::time::Instant;

struct Preparation {
    path: PathBuf,
    authorization: Authorization,
    delivery: DeliveryAuthorization,
    cancel: Arc<AtomicBool>,
    result: SyncSender<Result<OwnedDirectory, ()>>,
}

struct PendingPreparation {
    result: Receiver<Result<OwnedDirectory, ()>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for PendingPreparation {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}

impl Preparation {
    fn is_current(&self) -> bool {
        !self.cancel.load(Ordering::Acquire)
            && self.authorization.is_current()
            && self.delivery.job().event().received_at().elapsed()
                < among::plugin_notifications::EVENT_LIFETIME
            && self
                .delivery
                .origin_lease()
                .as_ref()
                .is_none_or(|lease| lease.is_current())
    }

    fn run(&self) -> Result<OwnedDirectory, ()> {
        use std::os::unix::fs::DirBuilderExt;
        if !self.is_current() {
            return Err(());
        }
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.path)
            .map_err(|_| ())?;
        if !self.is_current() {
            return Err(());
        }
        OwnedDirectory::open(&self.path).map_err(|_| ())
    }
}
static PREPARER: OnceLock<Option<SyncSender<Preparation>>> = OnceLock::new();

fn prepare(
    path: PathBuf,
    authorization: Authorization,
    delivery: DeliveryAuthorization,
) -> Option<PendingPreparation> {
    let sender = PREPARER
        .get_or_init(|| {
            let (send, receive) = sync_channel::<Preparation>(2);
            std::thread::Builder::new()
                .name("among-command-directory".into())
                .spawn(move || {
                    for request in receive {
                        let result = request.run();
                        let _ = request.result.try_send(result);
                    }
                })
                .ok()
                .map(|_| send)
        })
        .as_ref()?;
    let (result, receive) = sync_channel(1);
    let cancel = Arc::new(AtomicBool::new(false));
    sender
        .try_send(Preparation {
            path,
            authorization,
            delivery,
            cancel: cancel.clone(),
            result,
        })
        .ok()?;
    Some(PendingPreparation {
        result: receive,
        cancel,
    })
}

pub(super) struct Adapter {
    runtime: Option<Runtime>,
    channel: Option<Channel>,
    path: Option<PathBuf>,
    preparation: Option<PendingPreparation>,
    attempted: bool,
    stopped: bool,
    problem: Option<&'static str>,
}

impl Adapter {
    pub fn new() -> Self {
        let runtime = helper()
            .ok()
            .and_then(|path| Runtime::unprepared(&path).ok());
        let path = std::env::var_os("XDG_DATA_HOME")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
            .filter(|path| path.is_absolute())
            .map(|path| path.join("among/notification-command-home"));
        let problem = runtime.is_none().then_some("Command helper unavailable");
        Self {
            runtime,
            path,
            channel: None,
            preparation: None,
            attempted: false,
            stopped: false,
            problem,
        }
    }

    pub fn configure(&mut self, host: &mut Host, before: &Preferences, after: &Preferences) {
        if self.preparation.is_none()
            && !self.stopped
            && (before.configuration != after.configuration || before.command != after.command)
        {
            // only a new explicit edit can retry failed directory admission;
            // never replay the failed event or accumulate replacement readers.
            self.problem = None;
            self.attempted = false;
        }
        let Some(runtime) = &mut self.runtime else {
            self.problem = Some("Command helper unavailable");
            return;
        };
        let result = if let Some(configuration) = &after.configuration {
            match self.channel {
                Some(channel) => {
                    if before.configuration != after.configuration {
                        runtime.replace(host, channel, configuration.clone())
                    } else {
                        Ok(())
                    }
                }
                None => runtime
                    .register(host, configuration.clone())
                    .map(|channel| self.channel = Some(channel)),
            }
        } else {
            Ok(())
        };
        if result.is_err() {
            self.problem = Some("Command configuration could not be applied");
        }
        if let Some(channel) = self.channel {
            // configuration clearing keeps the same registration off; the next
            // accepted program revokes/replaces its previous generation.
            let policy = if after.configuration.is_some() && result.is_ok() {
                after.command
            } else {
                Policy::DELIVERY_DEFAULT
            };
            if (before.command != after.command || before.configuration != after.configuration)
                && runtime.set_policy(host, channel, policy).is_err()
            {
                self.problem = Some("Command policy could not be applied");
            }
        }
    }

    pub fn poll(
        &mut self,
        host: &std::rc::Rc<std::cell::RefCell<Host>>,
        bridge: &std::rc::Rc<std::cell::RefCell<among::plugin_notifications::Backend>>,
    ) {
        if let Some(receive) = &self.preparation {
            match receive.result.try_recv() {
                Ok(Ok(directory)) => {
                    if !self.stopped {
                        if let Some(runtime) = &mut self.runtime {
                            runtime.prepare(directory);
                        }
                    }
                    self.preparation = None;
                }
                Ok(Err(())) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.problem = Some("Owned command directory unavailable; no command started");
                    self.preparation = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let Some(runtime) = &mut self.runtime else {
            return;
        };
        if !self.stopped && !self.attempted && !runtime.is_prepared() {
            let now = Instant::now();
            let authority = self.channel.and_then(|channel| {
                let core = host.borrow();
                let authority = core.pending_jobs(channel).find_map(|job| {
                    let delivery = bridge.try_borrow().ok()?.authorize_delivery(job, now)?;
                    let authorization = core.authorization(job)?;
                    Some((authorization, delivery))
                });
                authority
            });
            if let Some((authorization, delivery)) = authority {
                self.attempted = true;
                self.preparation = self
                    .path
                    .clone()
                    .and_then(|path| prepare(path, authorization, delivery));
                if self.preparation.is_none() {
                    self.problem = Some("Owned command directory preparation unavailable");
                }
            }
        }
        if self.problem.is_some() {
            if let Some(channel) = self.channel {
                host.borrow_mut().discard_pending(channel);
            }
        }
        let now = Instant::now();
        runtime.poll_authorized(host, now, |job| {
            bridge.try_borrow().ok()?.authorize_delivery(job, now)
        });
    }

    pub fn shutdown(&mut self, host: &mut Host) {
        self.stopped = true;
        if let Some(preparation) = &self.preparation {
            preparation.cancel.store(true, Ordering::Release);
        }
        if let Some(runtime) = &mut self.runtime {
            runtime.shutdown(host);
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped && self.preparation.is_none() && commands::active_count() == 0
    }

    pub fn status(&self) -> String {
        if let Some(problem) = self.problem {
            return problem.into();
        }
        if self.preparation.is_some() {
            return "Preparing owned command directory off the UI loop".into();
        }
        "Local process completion is not remote delivery. Cancellation cannot undo external effects. Event kind and origin require current delivery authority.".into()
    }
}

#[cfg(not(test))]
fn helper() -> std::io::Result<PathBuf> {
    std::env::current_exe()
}

#[cfg(test)]
fn helper() -> std::io::Result<PathBuf> {
    // the Rust test harness is not the trusted application's main entrypoint.
    // native production-route checks use the separately built actual binary.
    let executable = std::env::current_exe()?;
    executable
        .parent()
        .and_then(|path| path.parent())
        .map(|path| path.join("among"))
        .ok_or_else(|| std::io::Error::other("test application executable unavailable"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use among::{
        notifications::{Eligibility, Focus},
        plugin_notifications::{Backend, EventInput, Owner, Target, EVENT_LIFETIME},
    };
    use std::{cell::RefCell, rc::Rc, time::Duration};

    #[test]
    fn preparation_reads_owned_authority_off_thread_before_creating_a_directory() {
        let root = std::env::temp_dir().join(format!(
            "among-command-preparation-{}-{}",
            std::process::id(),
            glib::uuid_string_random()
        ));
        std::fs::create_dir(&root).unwrap();
        for revoked in [
            "origin", "source", "channel", "session", "expiry", "cancel", "none",
        ] {
            let core = Rc::new(RefCell::new(Host::new().unwrap()));
            let source = core.borrow_mut().attach(1).unwrap();
            let channel = core
                .borrow_mut()
                .register_channel(Policy {
                    enabled: true,
                    eligibility: Eligibility::Always,
                })
                .unwrap();
            let mut backend = Backend::new(core.clone(), Focus::default());
            backend.bind("preparation-test");
            let owner = Owner {
                identity: "github:12/sample".into(),
                generation: "producer".into(),
            };
            backend.register_identity(&owner.identity).unwrap();
            let lease = backend.admit(owner.clone(), false, None).unwrap();
            let now = if revoked == "expiry" {
                Instant::now() - EVENT_LIFETIME
            } else {
                Instant::now()
            };
            backend
                .produce(
                    &owner,
                    EventInput {
                        sequence: 1,
                        target: Target::Pane { reference: 1 },
                        source_generation: Some(source.generation().to_string()),
                        message: "Synthetic".into(),
                        causal_job: None,
                    },
                    Some(1),
                    now,
                )
                .unwrap();
            let job = core.borrow_mut().take_ready(channel, now).unwrap();
            let authorization = core.borrow().authorization(job).unwrap();
            let delivery = backend.authorize_delivery(job, now).unwrap();
            let (result, _) = sync_channel(1);
            let path = root.join(revoked);
            let request = Preparation {
                path: path.clone(),
                authorization,
                delivery,
                cancel: Arc::new(AtomicBool::new(false)),
                result,
            };
            match revoked {
                "origin" => lease.revoke(),
                "source" => core.borrow_mut().detach(source),
                "channel" => core.borrow_mut().remove_channel(channel),
                "session" => *core.borrow_mut() = Host::new().unwrap(),
                "cancel" => request.cancel.store(true, Ordering::Release),
                _ => {}
            }
            let accepted = std::thread::spawn(move || request.run().is_ok())
                .join()
                .unwrap();
            assert_eq!(accepted, revoked == "none", "{revoked}");
            assert_eq!(
                path.exists(),
                revoked == "none",
                "invalid authority created a filesystem effect: {revoked}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_or_expired_backend_metadata_cannot_trigger_lazy_preparation() {
        for observed in [false, true] {
            let core = Rc::new(RefCell::new(Host::new().unwrap()));
            let bridge = Rc::new(RefCell::new(Backend::new(core.clone(), Focus::default())));
            let mut adapter = Adapter::new();
            let path = std::env::temp_dir().join(format!(
                "among-command-no-preparation-{}-{}",
                std::process::id(),
                glib::uuid_string_random()
            ));
            adapter.path = Some(path.clone());
            let preferences = Preferences {
                configuration: Some(
                    commands::Configuration::new(std::path::Path::new("/usr/bin/true"), Vec::new())
                        .unwrap(),
                ),
                command: Policy {
                    enabled: true,
                    eligibility: Eligibility::Always,
                },
                ..Preferences::default()
            };
            adapter.configure(
                &mut core.borrow_mut(),
                &Preferences::default(),
                &preferences,
            );
            let old = Instant::now() - EVENT_LIFETIME - Duration::from_secs(1);
            let event = core
                .borrow_mut()
                .application_event(Focus::default(), old)
                .unwrap();
            if observed {
                bridge
                    .borrow_mut()
                    .observe_host_event(event, None, Focus::default())
                    .unwrap();
            }
            assert_eq!(core.borrow().pending_count(adapter.channel), 1);
            adapter.poll(&core, &bridge);
            assert!(!adapter.attempted && adapter.preparation.is_none());
            assert!(
                !path.exists(),
                "missing/expired envelope prepared a directory"
            );
            adapter.shutdown(&mut core.borrow_mut());
        }
    }
}
