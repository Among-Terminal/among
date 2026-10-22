//! lazy filesystem work has one bounded process-wide worker, not one thread per
//! preferences window. neither inspecting nor accepting a configuration runs it.

use among::notifications::{
    commands::{self, OwnedDirectory, Runtime},
    preferences::Preferences,
    Channel, Host, Policy,
};
use std::path::PathBuf;
use std::sync::{
    mpsc::{sync_channel, Receiver, SyncSender},
    OnceLock,
};
use std::time::Instant;

struct Preparation {
    path: PathBuf,
    result: SyncSender<Result<OwnedDirectory, ()>>,
}
static PREPARER: OnceLock<Option<SyncSender<Preparation>>> = OnceLock::new();

fn prepare(path: PathBuf) -> Option<Receiver<Result<OwnedDirectory, ()>>> {
    let sender = PREPARER
        .get_or_init(|| {
            let (send, receive) = sync_channel::<Preparation>(2);
            std::thread::Builder::new()
                .name("among-command-directory".into())
                .spawn(move || {
                    use std::os::unix::fs::DirBuilderExt;
                    for request in receive {
                        let result = std::fs::DirBuilder::new()
                            .recursive(true)
                            .mode(0o700)
                            .create(&request.path)
                            .map_err(|_| ())
                            .and_then(|()| OwnedDirectory::open(&request.path).map_err(|_| ()));
                        let _ = request.result.try_send(result);
                    }
                })
                .ok()
                .map(|_| send)
        })
        .as_ref()?;
    let (result, receive) = sync_channel(1);
    sender.try_send(Preparation { path, result }).ok()?;
    Some(receive)
}

pub(super) struct Adapter {
    runtime: Option<Runtime>,
    channel: Option<Channel>,
    path: Option<PathBuf>,
    preparation: Option<Receiver<Result<OwnedDirectory, ()>>>,
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
            match receive.try_recv() {
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
        if !self.stopped
            && !self.attempted
            && !runtime.is_prepared()
            && self
                .channel
                .is_some_and(|channel| host.borrow().pending_count(Some(channel)) != 0)
        {
            self.attempted = true;
            self.preparation = self.path.clone().and_then(prepare);
            if self.preparation.is_none() {
                self.problem = Some("Owned command directory preparation unavailable");
            }
        }
        if self.problem.is_some() {
            if let Some(channel) = self.channel {
                host.borrow_mut().discard_pending(channel);
            }
        }
        let now = Instant::now();
        runtime.poll_guarded(host, now, |event| {
            // The API author's typed-core join must precede managed command
            // payloads. Never encode a managed event as an old terminal bell.
            bridge
                .borrow()
                .envelope(event, now)
                .is_some_and(|envelope| envelope.origin.is_none())
        });
    }

    pub fn shutdown(&mut self, host: &mut Host) {
        self.stopped = true;
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
        "Local process completion is not remote delivery. Cancellation cannot undo external effects. Managed-origin commands are withheld pending the typed-core integration.".into()
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
