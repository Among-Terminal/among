//! the host's side of confinement: choosing the level a start needs, building
//! the confinement, remembering what each generation proved about itself and
//! holding the user's consent to a reduced level.

use super::*;
use crate::plugin::sandbox::launch::{Applied, Confinement, Spec};
use crate::plugin::sandbox::level::{self, Decision, Probe, Protection};
use crate::plugin::sandbox::status::Enforcement;
use crate::plugin::sandbox::{data_dir, policy};
use std::path::PathBuf;
use std::process::Command;

/// What the host remembers per plugin about confinement.
#[derive(Default)]
pub(super) struct State {
    /// The reduced level the user allowed for this plugin identity.
    pub(super) consent: Option<Protection>,
    /// The level a start is waiting to be allowed to use, if one is.
    pub(super) awaiting: Option<Protection>,
    pub(super) record: Option<Record>,
}

/// One start's confinement, as the user can inspect it: what was asked, what
/// the child proved and why a start was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub identity: String,
    /// Host-side generation identifier of the process, once it exists.
    pub generation: Option<String>,
    pub grants: Vec<String>,
    pub digest: Option<String>,
    pub required: Option<Protection>,
    pub applied: Option<Applied>,
    pub refusal: Option<String>,
}

impl Record {
    /// The line Preferences shows for the protection level.
    pub fn protection_line(&self) -> String {
        match (&self.applied, &self.refusal) {
            (Some(applied), _) => {
                let mut line = format!("Protection: {}", applied.protection.label());
                if !applied.protection.is_full() {
                    line.push_str(" · running with reduced protection");
                }
                line
            }
            (None, Some(reason)) => format!("Not started: {reason}"),
            (None, None) => "Protection: waiting for the process to report".into(),
        }
    }

    /// The full record, one fact per line.
    pub fn details(&self) -> String {
        let mut lines = vec![format!("Identity: {}", self.identity)];
        if let Some(generation) = &self.generation {
            lines.push(format!("Generation: {generation}"));
        }
        lines.push(format!(
            "Grants: {}",
            if self.grants.is_empty() {
                "none".to_string()
            } else {
                self.grants.join(", ")
            }
        ));
        if let Some(digest) = &self.digest {
            lines.push(format!("Policy digest: {digest}"));
        }
        if let Some(applied) = &self.applied {
            let report = &applied.report;
            lines.push(format!(
                "Landlock: ABI {}, {}",
                report.landlock_abi,
                match report.enforcement {
                    Enforcement::None => "not enforced",
                    Enforcement::Partial => "partially enforced",
                    Enforcement::Full => "fully enforced",
                }
            ));
            lines.push(format!(
                "seccomp filter: {}",
                if report.seccomp_loaded {
                    "loaded"
                } else {
                    "not loaded"
                }
            ));
        }
        if let Some(reason) = &self.refusal {
            lines.push(format!("Refused: {reason}"));
        }
        lines.join("\n")
    }
}

/// A started or refused start.
pub(super) struct Launch {
    pub(super) command: Command,
    pub(super) confinement: Option<Confinement>,
}

fn unavailable(reason: impl ToString) -> PluginState {
    PluginState::Unavailable {
        reason: reason.to_string(),
    }
}

impl PluginHost {
    #[cfg(test)]
    pub(crate) fn confine_in_tests(&mut self, data_home: PathBuf) {
        self.test_confinement = Some(data_home);
    }

    #[cfg(test)]
    pub(crate) fn probe_in_tests(&mut self, probe: Probe) {
        self.test_probe = Some(probe);
    }

    fn current_probe(&self) -> Probe {
        #[cfg(test)]
        if let Some(probe) = self.test_probe {
            return probe;
        }
        level::probe()
    }

    fn current_data_home(&self) -> Option<PathBuf> {
        #[cfg(test)]
        if let Some(home) = &self.test_confinement {
            return Some(home.clone());
        }
        data_dir::data_home()
    }

    /// Builds the command for one start of `index`, confined unless this is a
    /// test host that was not asked to confine. An `Err` is the state to show.
    pub(super) fn launch_program(&mut self, index: usize) -> Result<Launch, PluginState> {
        let name = self.plugins[index].spec.name.clone();
        let entry = self.plugins[index]
            .managed
            .as_ref()
            .ok_or_else(|| unavailable("a validated managed installation is required"))?;
        let (directory, executable) = entry.executable_files().map_err(unavailable)?;
        let lease = entry.generation_lease();
        let namespace = entry.receipt.namespace_component();
        let plugin_id = entry.receipt.plugin_id.clone();
        // test hosts keep fixtures that write beside themselves working; the
        // production path below has no such switch.
        #[cfg(test)]
        if self.test_confinement.is_none() {
            let mut command = super::super::process::pinned_program(directory, executable, lease);
            environment::configure(&mut command, None).map_err(unavailable)?;
            return Ok(Launch {
                command,
                confinement: None,
            });
        }
        let features = self.plugins[index]
            .runtime
            .as_ref()
            .map(|runtime| runtime.grants.clone())
            .unwrap_or_default();
        let probe = self.current_probe();
        let consent = self.plugins[index].confinement.consent;
        let mut record = Record {
            identity: name.clone(),
            generation: None,
            grants: features.iter().cloned().collect(),
            digest: Some(policy::digest(&features)),
            required: None,
            applied: None,
            refusal: None,
        };
        let required = match level::decide(probe, consent) {
            Decision::Start { required } => required,
            Decision::Refuse { reason } => {
                return Err(self.refuse(index, record, None, reason));
            }
            Decision::NeedsConsent { offered } => {
                let reason = level::consent_message(offered, consent);
                return Err(self.refuse(index, record, Some(offered), reason));
            }
        };
        self.plugins[index].confinement.awaiting = None;
        record.required = Some(required);
        let Some(home) = self.current_data_home() else {
            return Err(self.refuse(
                index,
                record,
                None,
                "neither XDG_DATA_HOME nor HOME is set, so the plugin has no data directory".into(),
            ));
        };
        let data = match data_dir::open(&home, &namespace, &plugin_id) {
            Ok(data) => data,
            Err(problem) => {
                return Err(self.refuse(index, record, None, problem.to_string()));
            }
        };
        let spec = Spec {
            generation: &directory,
            data_dir: &data.directory,
            features: &features,
            probe,
            required,
        };
        let mut confinement = match Confinement::prepare(&spec) {
            Ok(confinement) => confinement,
            Err(refused) => return Err(self.refuse(index, record, None, refused.to_string())),
        };
        let mut command =
            super::super::process::confined_program(directory, executable, lease, &mut confinement);
        if let Err(reason) = environment::configure(&mut command, Some(&data.path)) {
            return Err(self.refuse(index, record, None, reason.into()));
        }
        self.plugins[index].confinement.record = Some(record);
        Ok(Launch {
            command,
            confinement: Some(confinement),
        })
    }

    fn refuse(
        &mut self,
        index: usize,
        mut record: Record,
        awaiting: Option<Protection>,
        reason: String,
    ) -> PluginState {
        eprintln!(
            "among: plugin `{}` was not started: {reason}",
            self.plugins[index].spec.name
        );
        record.refusal = Some(reason.clone());
        self.plugins[index].confinement.awaiting = awaiting;
        self.plugins[index].confinement.record = Some(record);
        unavailable(reason)
    }

    /// Records the generation identifier of the process just created.
    pub(super) fn note_generation(&mut self, index: usize, generation: &str) {
        if let Some(record) = self.plugins[index].confinement.record.as_mut() {
            record.generation = Some(generation.into());
        }
    }

    /// Copies what a started child proved into the plugin's record, once.
    pub(super) fn observe_confinement(&mut self, index: usize) {
        let Some(applied) = self.plugins[index]
            .io
            .as_ref()
            .and_then(|io| io.process.applied())
            .cloned()
        else {
            return;
        };
        let Some(record) = self.plugins[index].confinement.record.as_mut() else {
            return;
        };
        if record.applied.is_none() {
            eprintln!(
                "among: plugin `{}` confined: {} (policy {})",
                record.identity,
                applied.protection.label(),
                applied.digest
            );
            record.applied = Some(applied);
        }
    }

    /// Records why the child could not be confined.
    pub(super) fn record_refusal(&mut self, index: usize, reason: &str) {
        eprintln!(
            "among: plugin `{}` was not run: {reason}",
            self.plugins[index].spec.name
        );
        if let Some(record) = self.plugins[index].confinement.record.as_mut() {
            record.refusal = Some(reason.into());
        }
    }

    #[cfg(test)]
    pub(crate) fn confinement_record(&self, name: &str) -> Option<Record> {
        self.plugins[self.find(name)?].confinement.record.clone()
    }

    #[cfg(test)]
    pub(crate) fn consented_protection(&self, name: &str) -> Option<Protection> {
        self.plugins[self.find(name)?].confinement.consent
    }

    /// Records the user's consent to start `name` at `level` and tries the
    /// start again. Refused unless `level` is exactly what the latest refused
    /// start offered, so a stale dialog cannot grant a different level.
    pub fn allow_reduced_protection(
        &mut self,
        name: &str,
        level: Protection,
    ) -> Result<(), super::super::data::Problem> {
        let index = self.find(name).ok_or(super::super::data::Problem(
            "plugin is no longer registered",
        ))?;
        if self.plugins[index].confinement.awaiting != Some(level) || level.is_full() {
            return Err(super::super::data::Problem(
                "the offered protection level changed; review it again",
            ));
        }
        self.plugins[index].confinement.consent = Some(level);
        if self.plugins[index].spec.enabled && self.plugins[index].io.is_none() {
            self.start_by_name(name.to_string());
        }
        Ok(())
    }
}
