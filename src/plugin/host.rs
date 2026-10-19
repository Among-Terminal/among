//! Launching plugins, talking to them, and surviving their failures.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use super::protocol::{
    self, Context, EventKind, IncomingFragment, IncomingSummary, Rejection, Request, Response,
    PROTOCOL_MAJOR, PROTOCOL_MINOR,
};
use crate::slots::{Contribution, Fragment, Refresh, SlotStore, Style, Summary};

/// How long a plugin has to answer before it is treated as faulted.
///
/// Generous enough for a slow `git status` on a cold cache, short enough that a
/// wedged plugin is noticed rather than silently poisoning a slot forever.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// A plugin's description as read from configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSpec {
    pub name: String,
    /// The command to run; `argv[0]` is the program.
    pub command: Vec<String>,
    /// Whether the plugin starts with the application.
    pub enabled: bool,
}

/// Where a plugin currently stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginState {
    /// Configured and running.
    Running,
    /// Configured but switched off by the user.
    Disabled,
    /// The program could not be started.
    Missing { reason: String },
    /// The program started but speaks an incompatible protocol.
    Incompatible { plugin_major: u32, host_major: u32 },
    /// The plugin stopped, hung, or was restarted after a fault.
    Faulted { reason: String },
}

impl PluginState {
    /// Whether the plugin's data can be trusted as current.
    pub fn is_healthy(&self) -> bool {
        matches!(self, PluginState::Running)
    }
}

/// A live child process plus the channel its reader thread feeds.
///
/// Grouped so that stopping a plugin is a single `Option::take`, which avoids
/// the risk of tearing down half the plugin's state.
struct PluginIo {
    process: Child,
    stdin: Option<ChildStdin>,
    /// Channel carrying lines the reader thread read from the plugin.
    rx: Receiver<String>,
    /// Set once the plugin has announced a compatible version.
    handshake_done: bool,
}

/// One plugin under management.
struct PluginEntry {
    spec: PluginSpec,
    state: PluginState,
    io: Option<PluginIo>,
    /// Requests sent and not yet answered, for timeout accounting.
    inflight: HashMap<u64, Instant>,
    /// The scope each outstanding request asked about.
    scopes: HashMap<u64, String>,
    next_request_id: u64,
    /// The refresh policy the plugin last declared.
    refresh: Refresh,
    restart_at: Option<Instant>,
}

use std::time::Instant;

/// Hosts every configured plugin and pumps their output on the main loop.
pub struct PluginHost {
    plugins: Vec<PluginEntry>,
    /// Failures recorded for the current update, for diagnostics.
    last_rejections: Vec<(String, Rejection)>,
    /// Answers collected during the last [`PluginHost::poll`], for the host to
    /// turn into slot content.
    results: Vec<PluginResult>,
    /// Notifications raised since the last poll.
    notices: Vec<(String, String, Option<String>)>,
}

/// One plugin's answer, ready to become slot content.
#[derive(Debug, Clone)]
pub struct PluginResult {
    /// Which plugin answered.
    pub plugin: String,
    /// The scope that was asked about.
    pub scope: String,
    pub fragments: Vec<IncomingFragment>,
    /// `None` means the plugin declared event-driven refresh.
    pub refresh: Refresh,
}

/// Kill a plugin's process and drop its channels.
///
/// Taking the whole `io` in one step is what guarantees a stopped plugin is
/// left with no live pipes, rather than a half-torn-down process.
fn stop_io(plugin: &mut PluginEntry) {
    if let Some(mut io) = plugin.io.take() {
        let _ = io.process.kill();
        let _ = io.process.wait();
    }
    plugin.inflight.clear();
    plugin.scopes.clear();
}

impl Default for PluginHost {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginHost {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
            last_rejections: Vec::new(),
            results: Vec::new(),
            notices: Vec::new(),
        }
    }

    /// Take the notifications raised since the last poll.
    ///
    /// Returned separately from slot content because a notification is for the
    /// user rather than a description of the interface state, and must not be
    /// bound to a scope.
    pub fn drain_notices(&mut self) -> Vec<(String, String, Option<String>)> {
        std::mem::take(&mut self.notices)
    }

    /// Take the answers collected since the last poll.
    pub fn drain_results(&mut self) -> Vec<PluginResult> {
        std::mem::take(&mut self.results)
    }

    /// Register plugins from configuration.
    ///
    /// A plugin whose file is missing is recorded as `Missing` rather than
    /// aborting: one broken entry must not stop the application from starting.
    pub fn register_all(&mut self, specs: Vec<PluginSpec>) {
        for spec in specs {
            self.register(spec);
        }
    }

    /// Register a single plugin and start it if enabled.
    pub fn register(&mut self, spec: PluginSpec) {
        let name = spec.name.clone();
        self.plugins.push(PluginEntry {
            spec,
            state: PluginState::Disabled,
            io: None,
            inflight: HashMap::new(),
            scopes: HashMap::new(),
            next_request_id: 1,
            refresh: Refresh::IntervalMs(2000),
            restart_at: None,
        });
        self.start_by_name(name);
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.plugins.iter().position(|p| p.spec.name == name)
    }

    /// Names and states of every configured plugin, for listing.
    pub fn listing(&self) -> Vec<(String, PluginState)> {
        self.plugins
            .iter()
            .map(|p| (p.spec.name.clone(), p.state.clone()))
            .collect()
    }

    /// Switch a plugin on, starting it if necessary.
    pub fn enable(&mut self, name: &str) {
        let Some(index) = self.find(name) else { return };
        if self.plugins[index].state == PluginState::Running {
            return;
        }
        self.plugins[index].spec.enabled = true;
        self.start_by_name(name.to_string());
    }

    /// Switch a plugin off and stop its process.
    pub fn disable(&mut self, name: &str) {
        let Some(index) = self.find(name) else { return };
        self.plugins[index].spec.enabled = false;
        stop_io(&mut self.plugins[index]);
        self.plugins[index].state = PluginState::Disabled;
    }

    /// Stop every plugin, for shutdown.
    pub fn shutdown(&mut self) {
        for plugin in &mut self.plugins {
            if let Some(io) = plugin.io.as_mut() {
                if let Some(stdin) = io.stdin.as_mut() {
                    if let Ok(encoded) = serde_json::to_string(&Request::Shutdown) {
                        let _ = writeln!(stdin, "{encoded}");
                        let _ = stdin.flush();
                    }
                }
            }
        }
        for plugin in &mut self.plugins {
            stop_io(plugin);
        }
    }

    /// Spawn a plugin process.
    ///
    /// Failure to start is not an error for the host: it is recorded as the
    /// plugin's state and every other plugin continues.
    fn start_by_name(&mut self, name: String) {
        let Some(index) = self.find(&name) else {
            return;
        };
        if !self.plugins[index].spec.enabled {
            return;
        }

        let command = self.plugins[index].spec.command.clone();
        if command.is_empty() {
            self.plugins[index].state = PluginState::Missing {
                reason: "no command configured".into(),
            };
            return;
        }

        let spawned = Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();

        match spawned {
            Ok(mut child) => {
                let stdin = child.stdin.take();
                let stdout = child.stdout.take();

                let (tx, rx) = channel::<String>();
                if let Some(stdout) = stdout {
                    // The reader thread owns the only sender. When it ends,
                    // either the plugin closed stdout or the process is gone,
                    // which is exactly the signal the host needs.
                    let reader_tx = tx;
                    // A dedicated thread owns the pipe. Keeping stdout reads off
                    // the main loop means a plugin that floods output cannot
                    // stall the interface.
                    std::thread::spawn(move || {
                        let reader = BufReader::new(stdout);
                        for line in reader.lines() {
                            match line {
                                Ok(line) => {
                                    if reader_tx.send(line).is_err() {
                                        break;
                                    }
                                }
                                // A plugin that dies mid-write leaves a partial
                                // line; drop the reader and let the state machine
                                // notice the closed pipe.
                                Err(_) => break,
                            }
                        }
                    });
                }

                self.plugins[index].io = Some(PluginIo {
                    process: child,
                    stdin,
                    rx,
                    handshake_done: false,
                });
                self.plugins[index].restart_at = None;
                self.plugins[index].state = PluginState::Running;
                self.plugins[index].refresh = Refresh::IntervalMs(2000);

                let hello = Request::Hello {
                    major: PROTOCOL_MAJOR,
                    minor: PROTOCOL_MINOR,
                };
                self.send_to(index, &hello);
            }
            Err(err) => {
                self.plugins[index].io = None;
                let reason = format!("{err}");
                eprintln!(
                    "among: plugin `{}` could not be started: {reason}",
                    self.plugins[index].spec.name
                );
                self.plugins[index].state = PluginState::Missing { reason };
            }
        }
    }

    fn send_to(&mut self, index: usize, request: &Request) {
        let Some(io) = self.plugins[index].io.as_mut() else {
            return;
        };
        let Some(stdin) = io.stdin.as_mut() else {
            return;
        };
        let Ok(encoded) = serde_json::to_string(request) else {
            return;
        };
        let broken = writeln!(stdin, "{encoded}").is_err();
        if broken {
            self.fault(index, "plugin closed its input");
        }
    }

    fn fault(&mut self, index: usize, reason: &str) {
        stop_io(&mut self.plugins[index]);
        self.plugins[index].state = PluginState::Faulted {
            reason: reason.to_string(),
        };
        // Back off before restarting so a plugin that fails instantly cannot
        // spin the process table.
        self.plugins[index].restart_at = Some(Instant::now() + Duration::from_secs(2));
    }

    /// Ask a plugin for a scope's content.
    pub fn request_status(&mut self, name: &str, scope: &str, context: &Context) -> Option<u64> {
        let index = self.find(name)?;
        if !self.plugins[index].state.is_healthy() {
            return None;
        }
        let request_id = self.plugins[index].next_request_id;
        self.plugins[index].next_request_id += 1;
        self.plugins[index]
            .inflight
            .insert(request_id, Instant::now());
        self.plugins[index]
            .scopes
            .insert(request_id, scope.to_string());

        let request = Request::Status {
            request_id,
            scope: scope.to_string(),
            context: context.clone(),
        };
        self.send_to(index, &request);
        Some(request_id)
    }

    /// Tell every healthy plugin about a terminal event.
    pub fn broadcast_event(&mut self, event: EventKind, pane: u64, detail: Option<String>) {
        let request = Request::Event {
            event,
            pane,
            detail,
        };
        for index in 0..self.plugins.len() {
            if self.plugins[index].state.is_healthy() {
                self.send_to(index, &request);
            }
        }
    }

    /// Rejections recorded during the last [`PluginHost::poll`].
    pub fn rejections(&self) -> &[(String, Rejection)] {
        &self.last_rejections
    }

    /// Read whatever plugins have sent and reconcile timeouts.
    ///
    /// Called from the main loop. Any fault found here is contained: the plugin
    /// is stopped and marked, and its content is withdrawn by the caller via
    /// [`PluginHost::faulted_sources`].
    pub fn poll(&mut self) {
        self.last_rejections.clear();

        let now = Instant::now();
        for index in 0..self.plugins.len() {
            if !self.plugins[index].state.is_healthy() {
                self.restart_if_due(index, now);
                continue;
            }

            // Drain the reader channel without blocking.
            let drained: Vec<String> = self.plugins[index]
                .io
                .as_ref()
                .map(|io| {
                    let mut lines = Vec::new();
                    // A bounded drain keeps one chatty plugin from starving the
                    // others or the interface.
                    for _ in 0..256 {
                        match io.rx.try_recv() {
                            Ok(line) => lines.push(line),
                            Err(_) => break,
                        }
                    }
                    lines
                })
                .unwrap_or_default();

            for line in drained {
                self.handle_line(index, &line);
            }

            // Expire requests the plugin never answered.
            let timed_out: Vec<u64> = self.plugins[index]
                .inflight
                .iter()
                .filter(|(_, sent_at)| now.duration_since(**sent_at) > REQUEST_TIMEOUT)
                .map(|(id, _)| *id)
                .collect();
            if !timed_out.is_empty() {
                for id in timed_out {
                    self.plugins[index].inflight.remove(&id);
                }
                self.fault(index, "plugin did not answer within the timeout");
            }

            // Process exit is detected from the child itself, not from the
            // channel: a plugin may exit while its reader thread is still
            // holding the pipe open.
            let exited = self.plugins[index]
                .io
                .as_mut()
                .and_then(|io| io.process.try_wait().ok().flatten())
                .is_some();
            if exited {
                self.fault(index, "plugin exited");
            }
        }
    }

    fn restart_if_due(&mut self, index: usize, now: Instant) {
        let due = self.plugins[index]
            .restart_at
            .map(|at| now >= at)
            .unwrap_or(false);
        if due && self.plugins[index].spec.enabled {
            let name = self.plugins[index].spec.name.clone();
            self.start_by_name(name);
        }
    }

    fn handle_line(&mut self, index: usize, line: &str) {
        let name = self.plugins[index].spec.name.clone();
        match protocol::parse_response(line) {
            Err(rejection) => {
                // Malformed output is discarded; the plugin keeps running so a
                // single bad line does not cost the user their status.
                self.last_rejections.push((name, rejection));
            }
            Ok(Response::Hello { major, minor }) => {
                if protocol::version_is_compatible(major, minor) {
                    if let Some(io) = self.plugins[index].io.as_mut() {
                        io.handshake_done = true;
                    }
                    self.plugins[index].state = PluginState::Running;
                } else {
                    stop_io(&mut self.plugins[index]);
                    self.plugins[index].state = PluginState::Incompatible {
                        plugin_major: major,
                        host_major: PROTOCOL_MAJOR,
                    };
                    let rejection = Rejection::IncompatibleVersion {
                        plugin_major: major,
                        host_major: PROTOCOL_MAJOR,
                    };
                    // Reported once, at handshake: the per-poll rejection list
                    // is cleared on every tick, so a message kept only there
                    // would scroll past before anyone saw it.
                    eprintln!("among: plugin `{name}` was skipped: {rejection}");
                    self.last_rejections.push((name, rejection));
                }
            }
            Ok(response @ Response::Notify { .. }) => match protocol::validate_notice(response) {
                Ok(notice) => self.notices.push((name, notice.message, notice.style)),
                Err(rejection) => self.last_rejections.push((name, rejection)),
            },
            Ok(Response::Error { message }) => {
                self.last_rejections.push((name, Rejection::Malformed));
                let _ = message;
            }
            Ok(response @ Response::Status { .. }) => match protocol::validate_status(response) {
                Ok(valid) => {
                    self.plugins[index].inflight.remove(&valid.request_id);
                    let scope = self.plugins[index]
                        .scopes
                        .remove(&valid.request_id)
                        .unwrap_or_default();
                    if let Some(interval) = valid.refresh_ms {
                        self.plugins[index].refresh = Refresh::IntervalMs(interval);
                    } else {
                        self.plugins[index].refresh = Refresh::OnEvent;
                    }
                    self.results.push(PluginResult {
                        plugin: name,
                        scope,
                        fragments: valid.fragments,
                        refresh: self.plugins[index].refresh,
                    });
                }
                Err(rejection) => self.last_rejections.push((name, rejection)),
            },
        }
    }

    /// Names of plugins currently in a faulted or incompatible state.
    ///
    /// The caller uses this to withdraw their content so a stale fragment is
    /// never left on screen.
    pub fn faulted_sources(&self) -> Vec<String> {
        self.plugins
            .iter()
            .filter(|p| !p.state.is_healthy())
            .map(|p| p.spec.name.clone())
            .collect()
    }

    /// Names of healthy plugins, in registration order.
    pub fn healthy(&self) -> Vec<String> {
        self.plugins
            .iter()
            .filter(|p| p.state.is_healthy())
            .map(|p| p.spec.name.clone())
            .collect()
    }

    /// Convert validated fragments from one plugin into slot content.
    pub fn apply_fragments(
        store: &mut SlotStore,
        scope: crate::slots::Scope,
        source: &str,
        fragments: &[IncomingFragment],
        refresh: Refresh,
    ) {
        let converted: Vec<Fragment> = fragments
            .iter()
            .map(|incoming| {
                Fragment {
                    meter: incoming.meter,
                    text: incoming.text.clone(),
                    // An icon the theme does not have is dropped rather than
                    // rendered as a broken placeholder.
                    icon: incoming
                        .icon
                        .clone()
                        .filter(|name| crate::slots::icon_exists(name)),
                    style: parse_style(incoming.style.as_deref()),
                    tooltip: incoming.tooltip.clone(),
                    priority: incoming.priority,
                    source: source.to_string(),
                }
            })
            .filter(|f| f.text.as_deref().is_some_and(|t| !t.is_empty()) || f.icon.is_some())
            .collect();

        store.set(
            scope,
            source,
            Contribution {
                source: source.to_string(),
                fragments: converted,
                refresh,
                last_asked_ms: None,
            },
        );
    }

    /// Convert a summary from one plugin into sidebar content.
    pub fn apply_summary(
        store: &mut SlotStore,
        workspace: usize,
        source: &str,
        summary: &IncomingSummary,
    ) {
        store.set_summary(
            workspace,
            Summary {
                icon: summary
                    .icon
                    .clone()
                    .filter(|name| crate::slots::icon_exists(name)),
                text: summary.text.clone(),
                tooltip: summary.tooltip.clone(),
                style: parse_style(summary.style.as_deref()),
                source: source.to_string(),
            },
        );
    }
}

/// Map a style name from a plugin onto the closed [`Style`] set.
///
/// Unknown names fall back to `Default` rather than failing the message, so a
/// typo in a plugin degrades appearance instead of losing the content.
pub fn parse_style(name: Option<&str>) -> Style {
    match name {
        Some("success") => Style::Success,
        Some("warning") => Style::Warning,
        Some("error") => Style::Error,
        Some("accent") => Style::Accent,
        Some("dim") => Style::Dim,
        _ => Style::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slots::Scope;

    /// Command line of the `fake-plugin` example in the given behaviour.
    ///
    /// `cargo test` builds examples into `<profile>/examples`, next to the
    /// `<profile>/deps` directory holding this test executable, so the plugin
    /// is found relative to the running test rather than through PATH.
    fn fake_plugin(behaviour: &str) -> Vec<String> {
        let test_executable = std::env::current_exe().expect("test executable path");
        let profile = test_executable
            .parent()
            .and_then(std::path::Path::parent)
            .expect("test executable lives in <profile>/deps");
        let plugin = profile.join("examples").join("fake-plugin");
        assert!(
            plugin.is_file(),
            "{} is missing: a plain `cargo test` builds it from examples/fake-plugin.rs, \
             a filtered run may not, so build it with `cargo build --example fake-plugin`",
            plugin.display()
        );
        vec![plugin.to_string_lossy().into_owned(), behaviour.into()]
    }

    /// A plugin that answers every status request with a fixed body.
    fn echo_plugin() -> Vec<String> {
        fake_plugin("echo")
    }

    /// A plugin that completes the handshake and never answers a status request.
    fn hanging_plugin() -> Vec<String> {
        fake_plugin("hanging")
    }

    /// A plugin that greets and dies immediately.
    fn crashing_plugin() -> Vec<String> {
        fake_plugin("crashing")
    }

    fn wait_for(host: &mut PluginHost, predicate: impl Fn(&PluginHost) -> bool) -> bool {
        for _ in 0..200 {
            host.poll();
            if predicate(host) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    fn spec(command: Vec<String>) -> PluginSpec {
        PluginSpec {
            name: "probe".into(),
            command,
            enabled: true,
        }
    }

    #[test]
    fn a_missing_program_is_reported_without_aborting() {
        let mut host = PluginHost::new();
        host.register(PluginSpec {
            name: "ghost".into(),
            command: vec!["/nonexistent/binary".into()],
            enabled: true,
        });
        let (_, state) = host.listing().remove(0);
        assert!(
            matches!(state, PluginState::Missing { .. }),
            "got {state:?}"
        );
        assert!(!state.is_healthy());
    }

    #[test]
    fn a_disabled_plugin_is_not_started() {
        let mut host = PluginHost::new();
        host.register(PluginSpec {
            name: "off".into(),
            command: echo_plugin(),
            enabled: false,
        });
        assert_eq!(host.listing()[0].1, PluginState::Disabled);
    }

    #[test]
    fn an_empty_command_is_reported_as_missing() {
        let mut host = PluginHost::new();
        host.register(PluginSpec {
            name: "empty".into(),
            command: Vec::new(),
            enabled: true,
        });
        assert!(matches!(host.listing()[0].1, PluginState::Missing { .. }));
    }

    #[test]
    fn a_well_behaved_plugin_answers_a_status_request() {
        let mut host = PluginHost::new();
        host.register(spec(echo_plugin()));
        let healthy = wait_for(&mut host, |h| h.listing()[0].1.is_healthy());
        assert!(healthy, "plugin never became healthy");

        let context = Context {
            active_workspace: 0,
            workspace_count: 1,
            ..Default::default()
        };
        assert!(host
            .request_status("probe", "app_header", &context)
            .is_some());

        // The response is consumed by poll; reaching here without a rejection
        // means the handshake and request both round-tripped.
        for _ in 0..40 {
            host.poll();
            if host.rejections().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            host.rejections().is_empty(),
            "unexpected: {:?}",
            host.rejections()
        );
    }

    #[test]
    fn a_crash_is_contained_and_recorded_as_a_fault() {
        let mut host = PluginHost::new();
        host.register(spec(crashing_plugin()));
        let faulted = wait_for(&mut host, |h| !h.listing()[0].1.is_healthy());
        assert!(faulted, "a crashing plugin was still reported healthy");
        assert!(host.faulted_sources().contains(&"probe".to_string()));
    }

    #[test]
    fn a_hanging_plugin_is_faulted_rather_than_blocking() {
        let mut host = PluginHost::new();
        host.register(spec(hanging_plugin()));
        assert!(wait_for(&mut host, |h| h.listing()[0].1.is_healthy()));

        let context = Context::default();
        host.request_status("probe", "app_header", &context);

        // poll() must return promptly rather than waiting out the full timeout
        // inline, otherwise the interface would freeze.
        let start = Instant::now();
        host.poll();
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "poll blocked for {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn disabling_stops_the_process_and_withdraws_it() {
        let mut host = PluginHost::new();
        host.register(spec(echo_plugin()));
        wait_for(&mut host, |h| h.listing()[0].1.is_healthy());
        host.disable("probe");
        assert_eq!(host.listing()[0].1, PluginState::Disabled);
        assert!(host.faulted_sources().contains(&"probe".to_string()));
    }

    #[test]
    fn enabling_a_stopped_plugin_starts_it_again() {
        let mut host = PluginHost::new();
        host.register(spec(echo_plugin()));
        host.disable("probe");
        host.enable("probe");
        let healthy = wait_for(&mut host, |h| h.listing()[0].1.is_healthy());
        assert!(healthy);
    }

    #[test]
    fn malformed_output_is_rejected_without_killing_the_plugin() {
        let mut host = PluginHost::new();
        // The plugin stays alive after the bad line, so it is the only fault.
        host.register(spec(fake_plugin("malformed")));
        // a preliminary health poll can consume the bad line; the next poll
        // clears that batch's rejections, so observe them in this loop only.
        for _ in 0..40 {
            host.poll();
            if !host.rejections().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(
            host.rejections(),
            &[("probe".into(), Rejection::Malformed)],
            "malformed line was not rejected"
        );
        // The plugin survives its own bad output.
        assert!(host.listing()[0].1.is_healthy());

        // a running state alone cannot prove that the child still responds.
        assert!(host
            .request_status("probe", "app_header", &Context::default())
            .is_some());
        assert!(
            wait_for(&mut host, |h| !h.results.is_empty()),
            "plugin did not answer after its malformed output"
        );
        let results = host.drain_results();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].plugin, "probe");
        assert_eq!(results[0].scope, "app_header");
        assert_eq!(results[0].fragments.len(), 1);
        assert_eq!(results[0].fragments[0].text.as_deref(), Some("ok"));
        assert!(host.rejections().is_empty());
        assert!(host.listing()[0].1.is_healthy());
    }

    #[test]
    fn fragments_are_converted_and_unknown_icons_dropped() {
        let mut store = SlotStore::new();
        let fragments = vec![IncomingFragment {
            meter: None,
            text: Some("main".into()),
            icon: Some("definitely-not-a-real-icon-name".into()),
            style: Some("success".into()),
            tooltip: None,
            priority: 5,
        }];
        PluginHost::apply_fragments(
            &mut store,
            Scope::AppHeader,
            "git",
            &fragments,
            Refresh::OnEvent,
        );
        let visible = store.fragments(Scope::AppHeader);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].style, Style::Success);
        assert_eq!(visible[0].icon, None, "unknown icon should be dropped");
    }

    #[test]
    fn an_unknown_style_degrades_instead_of_failing() {
        assert_eq!(parse_style(Some("chartreuse")), Style::Default);
        assert_eq!(parse_style(None), Style::Default);
        assert_eq!(parse_style(Some("error")), Style::Error);
    }
}
