use super::*;
use among::notifications::{Eligibility, Focus, Host, Policy};
use backend::{Backend, DeliveryOutcome, Rejection as Refusal, Status};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

const KEY: &str = "github:12/sample";
const ROLES: &[&str] = &[
    "notifications.produce",
    "notifications.ui",
    "notifications.deliver",
    "context.terminals",
];

struct Fixture {
    host: PluginHost,
    backend: Rc<RefCell<Backend>>,
    core: Rc<RefCell<Host>>,
    root: PathBuf,
    package: PathBuf,
    command: u64,
}

impl Fixture {
    fn new(grants: &[&str], attached: bool, enabled: bool) -> Self {
        Self::with_manifest(grants, attached, enabled, |_, _| {})
    }

    fn with_manifest(
        grants: &[&str],
        attached: bool,
        enabled: bool,
        alter: impl FnOnce(&mut Value, &std::path::Path),
    ) -> Self {
        let root = std::env::var_os("AMONG_CHECK_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/notification-api-evidence")
            })
            .join(format!("notification-api-{}", glib::uuid_string_random()));
        let mut receipt = inventory::tests::fixture(&root);
        let package = receipt.package_path(&root);
        let binary = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("examples/notification-plugin");
        assert!(
            binary.is_file(),
            "build --example notification-plugin first"
        );
        std::fs::copy(binary, package.join("plugin")).unwrap();
        std::fs::set_permissions(
            package.join("plugin"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(package.join("among-plugin.json")).unwrap())
                .unwrap();
        manifest["required_features"] = json!(["status", "settings"]);
        manifest["optional_features"] = json!(ROLES);
        manifest["delivery_channels"] = json!([
            {"id":"first","name":"Same display","description":"Synthetic provider"},
            {"id":"second","name":"Same display","description":"Independent provider channel"}
        ]);
        alter(&mut manifest, &package);
        let bytes = manifest.to_string();
        std::fs::write(package.join("among-plugin.json"), &bytes).unwrap();
        receipt.manifest_sha256 = inventory::checksum(bytes.as_bytes()).unwrap();
        std::fs::write(
            root.join("installed.json"),
            json!({"version":1,"installations":[receipt]}).to_string(),
        )
        .unwrap();
        let entry = inventory::read(&root).unwrap().remove(0);
        let core = Rc::new(RefCell::new(Host::new().unwrap()));
        core.borrow_mut().attach(1).unwrap();
        core.borrow_mut().attach(2).unwrap();
        let backend = Rc::new(RefCell::new(Backend::new(core.clone(), Focus::default())));
        let mut host = PluginHost::with_notification_backend(
            Rc::new(Cell::new(true)),
            attached.then(|| backend.clone()),
        );
        host.update_context(
            vec![10, 20],
            Context {
                workspace_count: 2,
                active_workspace: 0,
                focused_pane: Some(1),
                terminals: vec![
                    protocol::TerminalInfo {
                        pane: 1,
                        workspace: 0,
                        cwd: Some("private-cwd-marker".into()),
                        title: Some("private-title-marker".into()),
                        running: true,
                    },
                    protocol::TerminalInfo {
                        pane: 2,
                        workspace: 1,
                        ..Default::default()
                    },
                ],
            },
        );
        let mut features = vec!["status", "settings"];
        features.extend(grants);
        host.register_managed(entry, Some(&json!({"enabled":enabled,"review":{"owner":"example","repository":"sample","features":features}})));
        Self {
            host,
            backend,
            core,
            root,
            package,
            command: 0,
        }
    }

    fn wait(&mut self, predicate: impl Fn(&Self) -> bool) {
        self.wait_for(Duration::from_secs(4), predicate);
    }

    fn wait_for(&mut self, timeout: Duration, predicate: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + timeout;
        loop {
            let start = Instant::now();
            self.host.poll();
            assert!(
                start.elapsed() < Duration::from_millis(250),
                "plugin polling blocked"
            );
            if predicate(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out: {:?}",
                self.host.listing()
            );
            std::thread::sleep(Duration::from_millis(3));
        }
    }

    fn admitted(&mut self) {
        self.wait(|fixture| fixture.host.configuration(KEY) == Configuration::Applied);
    }
    fn key(channel: &str) -> ChannelKey {
        ChannelKey {
            identity: KEY.into(),
            channel: channel.into(),
        }
    }
    fn policy(&mut self, channel: &str, eligibility: Eligibility, enabled: bool) {
        self.backend
            .borrow_mut()
            .set_policy(
                &Self::key(channel),
                Policy {
                    enabled,
                    eligibility,
                },
            )
            .unwrap();
    }
    fn received(&self) -> Vec<Value> {
        self.frames("received.jsonl")
    }
    fn frames(&self, name: &str) -> Vec<Value> {
        std::fs::read_to_string(self.package.join(name))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
    fn send(&mut self, message: Value) {
        self.command += 1;
        let temporary = self.package.join(format!("pending-{}.json", self.command));
        let destination = self.package.join(format!("input-{}.json", self.command));
        std::fs::write(&temporary, message.to_string()).unwrap();
        std::fs::rename(temporary, destination).unwrap();
        let command = self.command;
        self.wait(|fixture| {
            !fixture
                .package
                .join(format!("input-{command}.json"))
                .exists()
        });
        for _ in 0..3 {
            self.host.poll();
            std::thread::sleep(Duration::from_millis(3));
        }
    }
    fn event(&mut self, sequence: u64, pane: Option<u64>) {
        let source = pane.and_then(|pane| self.core.borrow().source(pane));
        self.send(json!({"type":"notification_event","session":"$session","generation":"$generation",
            "event":{"sequence":sequence,"target":pane.map_or(json!({"kind":"application"}), |pane| json!({"kind":"pane","reference":pane.to_string()})),
                "source_generation":source.map(|source| source.generation().to_string()),"message":"synthetic-notification"}}));
    }
    fn deliveries(&self) -> Vec<Value> {
        self.received()
            .into_iter()
            .filter(|frame| frame["type"] == "deliver")
            .collect()
    }
    fn contribution(&mut self, revision: u64, scope: Value) {
        let source = self.core.borrow().source(1).unwrap();
        self.send(json!({"type":"notification_contribution","session":"$session","generation":"$generation", "contribution":{
            "id":"notice","revision":revision,"scope":scope,"source_generation":source.generation().to_string(),"text":"safe plain text <b>not markup</b>",
            "icon":"theme:dialog-information-symbolic","style":"warning","actions":[{"id":"open","label":"Open"}]}}));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.host.shutdown();
        let end = Instant::now() + Duration::from_secs(3);
        while !self.host.is_stopped() && Instant::now() < end {
            self.host.poll();
            std::thread::sleep(Duration::from_millis(3));
        }
        assert!(self.host.is_stopped(), "owned fixture failed to reap");
        // retaining package-local transcripts is intentional test evidence, not
        // a claim that ordinary plugins may modify immutable installed packages.
        println!("notification fixture evidence: {}", self.root.display());
    }
}

#[test]
fn ordinary_host_does_not_advertise_roles_and_disabled_inventory_edit_never_starts_code() {
    let mut ordinary = Fixture::new(ROLES, false, true);
    ordinary.admitted();
    let hello = ordinary
        .received()
        .into_iter()
        .find(|frame| frame["type"] == "managed_hello")
        .unwrap();
    assert!(!hello.to_string().contains("notifications."));
    ordinary.event(1, None);
    assert_eq!(ordinary.core.borrow().event_count(), 0);
    let mut disabled = Fixture::new(ROLES, true, false);
    assert_eq!(disabled.backend.borrow().inventory().len(), 2);
    disabled.policy("first", Eligibility::Always, true);
    for _ in 0..10 {
        disabled.host.poll();
    }
    assert!(!disabled.package.join("executed").exists());
    assert!(!disabled.host.desired_enabled(KEY));
    assert!(disabled.backend.borrow().inventory()[0].policy.enabled);
    assert!(disabled.backend.borrow().inventory()[0].owner.is_none());
    disabled.host.enable(KEY);
    disabled.admitted();
    assert!(
        disabled.deliveries().is_empty(),
        "enable must not replay old work"
    );
}

#[test]
fn real_ingress_policies_identity_stamping_and_context_grants_change_actual_wire() {
    let mut fixture = Fixture::new(&["notifications.produce", "context.terminals"], true, true);
    fixture.admitted();
    // use a separately admitted provider identity, not self-echo delivery.
    fixture.add_provider(13, &["notifications.deliver", "context.terminals"]);
    fixture.provider_policy(13, "first", Eligibility::Always, true);
    fixture.provider_policy(
        13,
        "second",
        Eligibility::OriginatingTerminalUnfocused,
        true,
    );
    fixture.backend.borrow_mut().set_focus(Focus {
        application_active: true,
        viewed: fixture.core.borrow().source(1),
    });
    fixture
        .host
        .request_status(KEY, "app_header", &Context::default())
        .unwrap();
    fixture.wait(|fixture| !fixture.host.results.is_empty());
    let status = fixture
        .received()
        .into_iter()
        .find(|frame| frame["type"] == "status")
        .unwrap();
    assert!(status["context"]["terminals"][0]["notification_generation"].is_string());
    assert!(!status.to_string().contains("private-cwd-marker"));
    assert!(!status.to_string().contains("private-title-marker"));
    fixture.event(1, None);
    fixture.wait(|fixture| fixture.provider_deliveries(13).len() == 1);
    let targetless = fixture.provider_deliveries(13).remove(0);
    assert_eq!(targetless["delivery"]["channel"], "first");
    assert!(targetless["delivery"]["event"]["source"].is_null());
    assert_eq!(targetless["delivery"]["event"]["origin"]["identity"], KEY);
    assert_eq!(
        targetless["delivery"]["event"]["kind"],
        "managed_notification"
    );
    assert_eq!(
        fixture
            .core
            .borrow()
            .aggregate([1, 2], Instant::now())
            .unread_panes,
        0
    );
    std::thread::sleep(among::notifications::COOLDOWN);
    fixture.event(2, Some(2));
    fixture.wait(|fixture| fixture.provider_deliveries(13).len() == 3);
    let jobs = fixture.provider_deliveries(13);
    let event = &jobs[1]["delivery"]["event"];
    assert_eq!(event["source"]["pane"], "2");
    assert_eq!(event["source"]["workspace"], "20");
    assert_eq!(event["causal_id"], event["event_id"]);
    let unread = fixture.core.borrow().source(2).unwrap();
    assert!(
        fixture
            .core
            .borrow()
            .attention(unread, Instant::now())
            .unread
    );
    let count = fixture.core.borrow().event_count();
    fixture.send(json!({"type":"notify","message":"legacy diagnostic only"}));
    assert_eq!(fixture.core.borrow().event_count(), count);
    assert_eq!(
        fixture.host.drain_notices()[0].1,
        "Managed plugin diagnostic (payload omitted)"
    );
    assert_eq!(fixture.provider_deliveries(13).len(), 3);
}

impl Fixture {
    fn provider_package(&self, repository: u64) -> PathBuf {
        self.root
            .join(format!("provider-{repository}"))
            .join(format!(
                "generations/{repository}/sample/{}",
                "c".repeat(64)
            ))
    }
    fn add_provider(&mut self, repository: u64, grants: &[&str]) {
        let root = self.root.join(format!("provider-{repository}"));
        let mut receipt = inventory::tests::fixture(&root);
        let previous = receipt.package_path(&root);
        receipt.repository_id = repository;
        let package = receipt.package_path(&root);
        std::fs::create_dir_all(package.parent().unwrap()).unwrap();
        std::fs::set_permissions(
            package.parent().unwrap(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::set_permissions(
            package.parent().unwrap().parent().unwrap(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        std::fs::rename(previous, &package).unwrap();
        std::fs::copy(self.package.join("plugin"), package.join("plugin")).unwrap();
        let manifest = json!({"schema_version":1,"id":"sample","version":"1","name":"Provider","description":"Synthetic process","entrypoint":"plugin",
            "api":{"major":1,"minor":2},"required_features":["status","settings"],"optional_features":ROLES,
            "settings":{"version":1,"apply":"live","fields":[{"key":"label","label":"Label","kind":"string","default":"default"}]},
            "delivery_channels":[{"id":"first","name":"Same display","description":"Provider"},{"id":"second","name":"Same display","description":"Other"}]});
        std::fs::write(package.join("among-plugin.json"), manifest.to_string()).unwrap();
        receipt.manifest_sha256 = inventory::checksum(manifest.to_string().as_bytes()).unwrap();
        std::fs::write(
            root.join("installed.json"),
            json!({"version":1,"installations":[receipt]}).to_string(),
        )
        .unwrap();
        let entry = inventory::read(&root).unwrap().remove(0);
        let mut features = vec!["status", "settings"];
        features.extend(grants);
        self.host.register_managed(entry, Some(&json!({"enabled":true,"review":{"owner":"example","repository":"sample","features":features}})));
        self.wait(|fixture| {
            fixture
                .host
                .running_generation(&format!("github:{repository}/sample"))
                .is_some()
        });
    }
    fn provider_policy(
        &mut self,
        repository: u64,
        channel: &str,
        eligibility: Eligibility,
        enabled: bool,
    ) {
        self.backend
            .borrow_mut()
            .set_policy(
                &ChannelKey {
                    identity: format!("github:{repository}/sample"),
                    channel: channel.into(),
                },
                Policy {
                    enabled,
                    eligibility,
                },
            )
            .unwrap();
    }
    fn provider_deliveries(&self, repository: u64) -> Vec<Value> {
        std::fs::read_to_string(self.provider_package(repository).join("received.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|frame| frame["type"] == "deliver")
            .collect()
    }
}

#[test]
fn real_wrong_session_generation_target_causal_and_ungranted_events_have_no_authority() {
    let mut fixture = Fixture::new(&["notifications.produce"], true, true);
    fixture.admitted();
    for (session, generation, target) in [
        ("old-session", "$generation", json!({"kind":"application"})),
        ("$session", "old-generation", json!({"kind":"application"})),
        (
            "$session",
            "$generation",
            json!({"kind":"pane","reference":"1"}),
        ),
        (
            "$session",
            "$generation",
            json!({"kind":"workspace","reference":"10"}),
        ),
    ] {
        fixture.send(json!({"type":"notification_event","session":session,"generation":generation,"event":{"sequence":1,"target":target,"message":"unauthorized"}}));
    }
    fixture.send(json!({"type":"notification_event","session":"$session","generation":"$generation","event":{"sequence":1,"target":{"kind":"application"},"message":"feedback","causal_job":1}}));
    assert_eq!(fixture.core.borrow().event_count(), 0);
    fixture.event(1, None);
    assert_eq!(fixture.core.borrow().event_count(), 1);
    fixture.event(1, None);
    fixture.event(2, None);
    assert_eq!(fixture.core.borrow().event_count(), 1);
    assert!(fixture
        .backend
        .borrow()
        .rejections()
        .contains(&Refusal::RateLimited));
    assert!(fixture
        .backend
        .borrow()
        .rejections()
        .contains(&Refusal::CausalFeedback));
    let mut ungranted = Fixture::new(&[], true, true);
    ungranted.admitted();
    ungranted.event(1, None);
    assert_eq!(ungranted.core.borrow().event_count(), 0);
}

#[test]
fn contributions_route_real_actions_only_to_current_owner_and_withdraw_stale_state() {
    let mut fixture = Fixture::new(&["notifications.ui", "context.terminals"], true, true);
    fixture.admitted();
    let scope = json!({"slot":"pane_header","target":{"kind":"pane","reference":"1"}});
    fixture.contribution(1, scope.clone());
    let items = fixture.backend.borrow().contributions(Instant::now());
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].text, "safe plain text <b>not markup</b>");
    let old = items[0].actions[0].handle.clone();
    fixture
        .backend
        .borrow_mut()
        .activate(&old, Instant::now())
        .unwrap();
    fixture.wait(|fixture| {
        fixture
            .received()
            .iter()
            .any(|frame| frame["type"] == "notification_action")
    });
    let action = fixture
        .received()
        .into_iter()
        .find(|frame| frame["type"] == "notification_action")
        .unwrap();
    assert_eq!(action["action"], "open");
    assert_eq!(action["contribution"], "notice");
    fixture.contribution(2, scope.clone());
    assert_eq!(
        fixture.backend.borrow_mut().activate(&old, Instant::now()),
        Err(Refusal::Stale)
    );
    let current = fixture.backend.borrow().contributions(Instant::now())[0].actions[0]
        .handle
        .clone();
    fixture.host.disable(KEY);
    assert!(fixture
        .backend
        .borrow()
        .contributions(Instant::now())
        .is_empty());
    assert_eq!(
        fixture
            .backend
            .borrow_mut()
            .activate(&current, Instant::now()),
        Err(Refusal::Stale)
    );
    fixture.wait(|fixture| fixture.host.is_stopped());
    fixture.host.enable(KEY);
    fixture.admitted();
    fixture.contribution(1, scope);
    let source = fixture.core.borrow().source(1).unwrap();
    fixture.core.borrow_mut().detach(source);
    assert!(fixture
        .backend
        .borrow()
        .contributions(Instant::now())
        .is_empty());
    for (revision, slot, icon, text) in [
        (2, "title", "theme:folder-symbolic", "bad"),
        (3, "restore", "theme:folder-symbolic", "bad"),
        (4, "pane_header", "/outside/icon.png", "bad"),
        (
            5,
            "pane_header",
            "theme:folder-symbolic",
            "\u{1b}[31mcontrol",
        ),
    ] {
        fixture.send(json!({"type":"notification_contribution","session":"$session","generation":"$generation","contribution":{
            "id":"notice","revision":revision,"scope":{"slot":slot,"target":{"kind":"pane","reference":"1"}},"source_generation":source.generation().to_string(),"text":text,"icon":icon}}));
    }
    assert!(fixture
        .backend
        .borrow()
        .contributions(Instant::now())
        .is_empty());
}

#[test]
fn provider_results_refusal_double_timeout_cancellation_and_disable_do_not_replay() {
    let mut fixture = Fixture::new(&["notifications.produce"], true, true);
    fixture.admitted();
    fixture.add_provider(13, &["notifications.deliver"]);
    fixture.provider_policy(13, "first", Eligibility::Always, true);
    std::fs::write(
        fixture.provider_package(13).join("delivery-mode"),
        "refused",
    )
    .unwrap();
    fixture.event(1, None);
    fixture.wait(|fixture| {
        fixture
            .backend
            .borrow()
            .completions()
            .iter()
            .any(|completion| completion.status == Status::Result(DeliveryOutcome::Refused))
    });
    assert_eq!(fixture.backend.borrow().active_count(), 0);
    assert!(fixture.provider_deliveries(13)[0]["delivery"]["event"]["source"].is_null());
    std::thread::sleep(among::notifications::COOLDOWN);
    std::fs::write(fixture.provider_package(13).join("delivery-mode"), "double").unwrap();
    fixture.event(2, None);
    fixture.wait(|fixture| fixture.backend.borrow().completions().len() == 2);
    for _ in 0..10 {
        fixture.host.poll();
    }
    assert_eq!(fixture.backend.borrow().completions().len(), 2);
    std::thread::sleep(among::notifications::COOLDOWN);
    std::fs::write(fixture.provider_package(13).join("delivery-mode"), "late").unwrap();
    fixture.event(3, None);
    fixture.wait(|fixture| fixture.backend.borrow().active_count() == 1);
    fixture.provider_policy(13, "first", Eligibility::Always, false);
    assert_eq!(fixture.backend.borrow().active_count(), 0);
    assert_eq!(
        fixture.host.plugins[1]
            .runtime
            .as_ref()
            .unwrap()
            .deliveries
            .len(),
        1,
        "cancelled result retains a shared reply credit"
    );
    fixture.wait(|fixture| {
        std::fs::read_to_string(fixture.provider_package(13).join("received.jsonl"))
            .unwrap_or_default()
            .contains("cancel_delivery")
    });
    let count = fixture.backend.borrow().completions().len();
    std::fs::write(
        fixture.provider_package(13).join("release-deliveries"),
        "yes",
    )
    .unwrap();
    fixture.wait(|fixture| {
        fixture.host.plugins[1]
            .runtime
            .as_ref()
            .unwrap()
            .deliveries
            .is_empty()
    });
    assert_eq!(fixture.backend.borrow().completions().len(), count);
    fixture.provider_policy(13, "first", Eligibility::Always, true);
    fixture.host.disable("github:13/sample");
    assert!(
        fixture
            .backend
            .borrow()
            .inventory()
            .iter()
            .find(|row| row.declaration.key.identity == "github:13/sample"
                && row.declaration.key.channel == "first")
            .unwrap()
            .policy
            .enabled
    );
    fixture.wait(|fixture| fixture.host.plugins[1].io.is_none());
    let delivered = fixture.provider_deliveries(13).len();
    fixture.host.enable("github:13/sample");
    fixture.wait(|fixture| {
        fixture
            .host
            .running_generation("github:13/sample")
            .is_some()
    });
    assert_eq!(fixture.provider_deliveries(13).len(), delivered);
    // a fresh job really stalls for the finite deadline, not an artificial clock.
    std::fs::remove_file(fixture.provider_package(13).join("release-deliveries")).unwrap();
    std::fs::write(fixture.provider_package(13).join("delivery-mode"), "stall").unwrap();
    std::thread::sleep(among::notifications::COOLDOWN);
    let start = Instant::now();
    fixture.event(4, None);
    fixture.wait_for(Duration::from_secs(7), |fixture| {
        fixture
            .backend
            .borrow()
            .completions()
            .iter()
            .any(|completion| completion.status == Status::TimedOutUncertain)
    });
    assert!(start.elapsed() >= backend::DEADLINE);
    assert_eq!(fixture.core.borrow().event_count(), 4);
}

#[test]
fn cancelled_jobs_share_fifteen_retained_credits_and_preserve_reserved_enumeration() {
    let mut fixture = Fixture::new(&["notifications.deliver", "context.terminals"], true, true);
    fixture.admitted();
    fixture.policy("first", Eligibility::Always, true);
    std::fs::write(fixture.package.join("delivery-mode"), "late").unwrap();
    for id in 1..=15 {
        let source = fixture.core.borrow_mut().attach(100 + id).unwrap();
        let event = fixture
            .core
            .borrow_mut()
            .bell(source, Focus::default(), Instant::now())
            .unwrap();
        fixture
            .backend
            .borrow_mut()
            .observe_host_event(event, Some(10))
            .unwrap();
        fixture.wait(|fixture| fixture.deliveries().len() == id as usize);
        fixture.policy("first", Eligibility::Always, false);
        fixture.policy("first", Eligibility::Always, true);
        assert_eq!(
            fixture.host.plugins[0]
                .runtime
                .as_ref()
                .unwrap()
                .deliveries
                .len(),
            id as usize
        );
    }
    assert!(fixture
        .host
        .request_status(KEY, "app_header", &Context::default())
        .is_none());
    fixture.send(
        json!({"type":"enumerate","session":"$session","generation":"$generation","request_id":900,
        "scope":{"slot":"app_header","target":{"kind":"application"}},"cursor":null}),
    );
    fixture.wait(|fixture| {
        fixture
            .received()
            .iter()
            .any(|frame| frame["type"] == "context_page")
    });
    assert_eq!(
        fixture.host.plugins[0]
            .runtime
            .as_ref()
            .unwrap()
            .deliveries
            .len(),
        15
    );
    let count = fixture.backend.borrow().completions().len();
    std::fs::write(fixture.package.join("release-deliveries"), "yes").unwrap();
    fixture.wait(|fixture| {
        fixture.host.plugins[0]
            .runtime
            .as_ref()
            .unwrap()
            .deliveries
            .is_empty()
    });
    assert_eq!(fixture.backend.borrow().completions().len(), count);
    assert!(fixture.host.plugins[0].state.is_healthy());
    fixture
        .host
        .request_status(KEY, "app_header", &Context::default())
        .unwrap();
    fixture.wait(|fixture| !fixture.host.results.is_empty());
}

#[test]
fn cyclic_dual_role_processes_cannot_hide_feedback_and_flood_is_contained() {
    let mut fixture = Fixture::new(
        &["notifications.produce", "notifications.deliver"],
        true,
        true,
    );
    fixture.admitted();
    fixture.add_provider(13, &["notifications.produce", "notifications.deliver"]);
    fixture.provider_policy(13, "first", Eligibility::Always, true);
    fixture.policy("first", Eligibility::Always, true);
    std::fs::write(fixture.provider_package(13).join("delivery-mode"), "cycle").unwrap();
    fixture.event(1, None);
    fixture.wait(|fixture| {
        fixture
            .backend
            .borrow()
            .rejections()
            .contains(&Refusal::CausalFeedback)
    });
    assert_eq!(fixture.core.borrow().event_count(), 1);
    // the provider also attempts to disguise feedback as a fresh unsolicited
    // event with no causal field; the host, not that field, remembers delivery.
    std::fs::write(
        fixture.provider_package(13).join("input-1.json"),
        json!({"type":"notification_event","session":"$session","generation":"$generation",
        "event":{"sequence":1001,"target":{"kind":"application"},"message":"hidden-feedback"}})
        .to_string(),
    )
    .unwrap();
    fixture.wait(|fixture| !fixture.provider_package(13).join("input-1.json").exists());
    for _ in 0..10 {
        fixture.host.poll();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(fixture.core.borrow().event_count(), 1);
    assert_eq!(fixture.provider_deliveries(13).len(), 1);
    assert!(
        fixture.deliveries().is_empty(),
        "self echo or cyclic job was delivered"
    );
    std::thread::sleep(among::notifications::COOLDOWN);
    std::fs::write(fixture.provider_package(13).join("delivery-mode"), "flood").unwrap();
    fixture.event(2, None);
    fixture.wait(|fixture| !fixture.host.plugins[1].state.is_healthy());
    assert!(fixture.host.plugins[0].state.is_healthy());
    fixture
        .host
        .request_status(KEY, "app_header", &Context::default())
        .unwrap();
    fixture.wait(|fixture| !fixture.host.results.is_empty());
    assert!(fixture.backend.borrow().completions().len() <= 32);
    assert!(fixture.backend.borrow().rejections().len() <= 32);
    assert_eq!(fixture.core.borrow().event_count(), 2);
}

#[test]
fn static_raster_snapshot_is_used_by_real_contribution_and_invalid_wire_artwork_is_refused() {
    let mut fixture =
        Fixture::with_manifest(&["notifications.ui"], true, true, |manifest, package| {
            let image =
                gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, 2, 3).unwrap();
            image.fill(0x336699ff);
            std::fs::write(
                package.join("icon.png"),
                image.save_to_bufferv("png", &[]).unwrap(),
            )
            .unwrap();
            std::fs::set_permissions(
                package.join("icon.png"),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            manifest["icon"] = json!("icon.png");
        });
    fixture.admitted();
    fixture.send(json!({"type":"notification_contribution","session":"$session","generation":"$generation","contribution":{
        "id":"notice","revision":1,"scope":{"slot":"app_header","target":{"kind":"application"}},"text":"Snapshot","icon":"package"}}));
    let items = fixture.backend.borrow().contributions(Instant::now());
    assert!(
        matches!(&items[0].icon, Some(Icon::Raster(bytes)) if bytes.starts_with(b"\x89PNG\r\n\x1a\n"))
    );
    for (revision, icon, text) in [
        (2, "/outside/icon.png", "bad path"),
        (3, "theme:bad/name", "bad theme"),
        (4, "theme:folder-symbolic", "\u{1b}[31mbad control"),
    ] {
        fixture.send(json!({"type":"notification_contribution","session":"$session","generation":"$generation","contribution":{
            "id":"notice","revision":revision,"scope":{"slot":"app_header","target":{"kind":"application"}},"text":text,"icon":icon}}));
    }
    assert_eq!(
        fixture.backend.borrow().contributions(Instant::now())[0].revision,
        1
    );
    assert!(fixture
        .backend
        .borrow()
        .rejections()
        .contains(&Refusal::InvalidData));
    fixture
        .host
        .stored_settings_changed(KEY, json!({"version":1,"values":{"label":"non-default"}}));
    fixture.wait(|fixture| fixture.host.configuration(KEY) == Configuration::Applied);
    assert!(fixture
        .backend
        .borrow()
        .contributions(Instant::now())
        .is_empty());
}

#[test]
#[ignore = "requires among-tools ci-x11, private HOME/XDG, real PTYs and xdotool"]
fn native_pty_bell_real_provider_and_keyboard_remain_responsive_during_flood() {
    use gtk::prelude::*;
    use vte::prelude::*;
    let config = PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap());
    crate::test_isolation::assert_below_check_root(&config);
    std::env::set_var("SHELL", "/bin/sh");
    adw::init().unwrap();
    let mut fixture = Fixture::new(&["notifications.deliver"], true, true);
    fixture.admitted();
    fixture.policy("first", Eligibility::Always, true);
    let window = gtk::Window::builder()
        .title("Among notification API isolated test")
        .default_width(800)
        .default_height(500)
        .build();
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    window.set_child(Some(&row));
    let spawned = Rc::new(Cell::new(0));
    let exited = Rc::new(Cell::new(0));
    let mut terminals = Vec::new();
    for pane in 1..=2 {
        let pty = crate::new_pty().unwrap();
        let terminal = vte::Terminal::builder()
            .pty(&pty)
            .hexpand(true)
            .vexpand(true)
            .build();
        terminal.set_audible_bell(false);
        let core = fixture.core.clone();
        let backend = fixture.backend.clone();
        let source = core.borrow().source(pane).unwrap();
        terminal.connect_bell(move |_| {
            let event = core
                .borrow_mut()
                .bell(source, Focus::default(), Instant::now())
                .unwrap();
            backend
                .borrow_mut()
                .observe_host_event(event, Some(if pane == 1 { 10 } else { 20 }))
                .unwrap();
        });
        let done = spawned.clone();
        let ended = exited.clone();
        terminal.connect_child_exited(move |_, _| ended.set(ended.get() + 1));
        crate::spawn_shell(
            &pty,
            &terminal,
            Some(fixture.root.display().to_string()),
            false,
            Rc::new(move |result| {
                result.unwrap();
                done.set(done.get() + 1);
            }),
        );
        row.append(&terminal);
        terminals.push(terminal);
    }
    window.present();
    let pump = |fixture: &mut Fixture, predicate: &dyn Fn(&Fixture) -> bool, timeout: Duration| {
        let end = Instant::now() + timeout;
        loop {
            while glib::MainContext::default().pending() {
                glib::MainContext::default().iteration(false);
            }
            fixture.host.poll();
            if predicate(fixture) {
                break;
            }
            assert!(
                Instant::now() < end,
                "native notification boundary timed out"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    pump(
        &mut fixture,
        &|_| spawned.get() == 2 && window.is_mapped(),
        Duration::from_secs(5),
    );
    let xdo = |args: &[&str]| {
        let output = std::process::Command::new("xdotool")
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let ids = xdo(&[
        "search",
        "--onlyvisible",
        "--pid",
        &std::process::id().to_string(),
        "--name",
        "Among notification API isolated test",
    ]);
    let id = ids.lines().last().unwrap();
    xdo(&["windowactivate", "--sync", id]);
    xdo(&["windowfocus", "--sync", id]);
    terminals[1].grab_focus();
    pump(
        &mut fixture,
        &|_| terminals[1].has_focus() && window.is_active(),
        Duration::from_secs(2),
    );
    terminals[0].feed_child(b"printf '\\a'\n");
    pump(
        &mut fixture,
        &|fixture| !fixture.backend.borrow().completions().is_empty(),
        Duration::from_secs(3),
    );
    assert_eq!(
        fixture.deliveries()[0]["delivery"]["event"]["kind"],
        "terminal_bell"
    );
    assert!(
        fixture.deliveries()[0]["delivery"]["event"]["source"].is_null(),
        "no terminal context grant"
    );
    std::fs::write(fixture.package.join("delivery-mode"), "flood").unwrap();
    // a different actual bell source bypasses only the repeated-source cooldown,
    // not policy, supervised I/O or provider result handling.
    terminals[1].feed_child(b"printf '\\a'\n");
    let start = Instant::now();
    xdo(&[
        "type",
        "--clearmodifiers",
        "--delay",
        "0",
        "printf responsive > \"$HOME/notification-api-input\"",
    ]);
    xdo(&["key", "--clearmodifiers", "Return"]);
    let marker = PathBuf::from(std::env::var_os("HOME").unwrap()).join("notification-api-input");
    pump(&mut fixture, &|_| marker.exists(), Duration::from_secs(1));
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "responsive");
    assert!(start.elapsed() < Duration::from_secs(1));
    pump(
        &mut fixture,
        &|fixture| !fixture.host.plugins[0].state.is_healthy(),
        Duration::from_secs(3),
    );
    for terminal in &terminals {
        terminal.feed_child(b"exit\n");
    }
    pump(&mut fixture, &|_| exited.get() == 2, Duration::from_secs(3));
    window.destroy();
    println!("PASS semantic VTE bell -> attached core -> actual provider; real sibling keyboard input {:?}; provider flood contained, PTYs exited", start.elapsed());
}

#[test]
fn required_roles_refuse_execution_without_bridge_and_actual_pre_admission_ingress_is_rejected() {
    let unavailable =
        Fixture::with_manifest(&["notifications.produce"], false, true, |manifest, _| {
            manifest["required_features"] = json!(["status", "settings", "notifications.produce"]);
            manifest["optional_features"] = json!([]);
        });
    assert!(matches!(
        unavailable.host.listing()[0].1,
        PluginState::Unavailable { .. }
    ));
    assert!(!unavailable.package.join("executed").exists());
    let mut early = Fixture::with_manifest(&["notifications.produce"], true, true, |_, package| {
        std::fs::write(
            package.join("pre-admission.json"),
            json!({"type":"notification_event","session":"$session","generation":"$generation",
            "event":{"sequence":1,"target":{"kind":"application"},"message":"premature"}})
            .to_string(),
        )
        .unwrap();
    });
    early.admitted();
    assert_eq!(early.core.borrow().event_count(), 0);
    assert!(early
        .backend
        .borrow()
        .rejections()
        .contains(&Refusal::Unauthorized));
    early.event(1, None);
    assert_eq!(early.core.borrow().event_count(), 1);
    let mut stale = Fixture::new(&["notifications.produce", "context.terminals"], true, true);
    stale.admitted();
    stale.send(json!({"type":"notification_event","session":"$session","generation":"$generation","event":{
        "sequence":1,"target":{"kind":"pane","reference":"1"},"source_generation":"0","message":"stale source"}}));
    assert_eq!(stale.core.borrow().event_count(), 0);
    assert!(stale
        .backend
        .borrow()
        .rejections()
        .contains(&Refusal::Stale));
}

#[test]
fn actual_provider_failed_uncertain_and_accepted_outcomes_are_distinct_and_uninstall_cannot_resurrect_authority(
) {
    let mut fixture = Fixture::new(
        &[
            "notifications.deliver",
            "notifications.produce",
            "notifications.ui",
        ],
        true,
        true,
    );
    fixture.admitted();
    fixture.policy("first", Eligibility::Always, true);
    for (index, (mode, outcome)) in [
        ("failed", DeliveryOutcome::Failed),
        ("uncertain", DeliveryOutcome::Uncertain),
        ("accepted", DeliveryOutcome::Accepted),
    ]
    .into_iter()
    .enumerate()
    {
        std::fs::write(fixture.package.join("delivery-mode"), mode).unwrap();
        let source = fixture
            .core
            .borrow_mut()
            .attach(100 + index as u64)
            .unwrap();
        let event = fixture
            .core
            .borrow_mut()
            .bell(source, Focus::default(), Instant::now())
            .unwrap();
        fixture
            .backend
            .borrow_mut()
            .observe_host_event(event, Some(10))
            .unwrap();
        fixture.wait(|fixture| fixture.backend.borrow().completions().len() == index + 1);
        assert_eq!(
            fixture
                .backend
                .borrow()
                .completions()
                .back()
                .unwrap()
                .status,
            Status::Result(outcome)
        );
    }
    fixture.backend.borrow_mut().uninstall(KEY);
    fixture.event(1, None);
    assert_eq!(fixture.core.borrow().event_count(), 3);
    let generation = fixture.host.running_generation(KEY).unwrap();
    let executed = std::fs::read(fixture.package.join("executed")).unwrap();
    assert!(fixture.host.restart_managed(KEY, &generation));
    fixture.wait_for(Duration::from_secs(5), |fixture| matches!(&fixture.host.plugins[0].state,
        PluginState::Unavailable { reason } if reason.contains("notification registration was revoked")));
    assert!(!fixture.host.plugins[0].state.is_healthy());
    assert_eq!(
        std::fs::read(fixture.package.join("executed")).unwrap(),
        executed,
        "a retired registration cannot re-execute old code merely to fail its handshake"
    );
    assert!(fixture
        .backend
        .borrow()
        .inventory()
        .iter()
        .all(|row| !row.installed && row.owner.is_none()));
    assert_eq!(fixture.core.borrow().event_count(), 3);
}

#[test]
fn disable_revokes_live_action_authority_even_while_a_renderer_snapshot_borrow_is_held() {
    let mut fixture = Fixture::new(&["notifications.ui"], true, true);
    fixture.admitted();
    fixture.contribution(
        1,
        json!({"slot":"app_header","target":{"kind":"application"}}),
    );
    let handle = fixture.backend.borrow().contributions(Instant::now())[0].actions[0]
        .handle
        .clone();
    let snapshot = fixture.backend.borrow();
    fixture.host.disable(KEY);
    assert!(
        snapshot.contributions(Instant::now()).is_empty(),
        "revocation must not depend on acquiring the renderer's RefCell borrow"
    );
    drop(snapshot);
    assert_eq!(
        fixture
            .backend
            .borrow_mut()
            .activate(&handle, Instant::now()),
        Err(Refusal::Stale)
    );
    fixture.wait(|fixture| fixture.host.is_stopped());
    assert!(!fixture
        .received()
        .iter()
        .any(|frame| frame["type"] == "notification_action"));
}
