use super::*;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::Mutex;

pub(crate) static JOB_TESTS: Mutex<()> = Mutex::new(());

pub(crate) struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    declared: Option<usize>,
    chunked: bool,
    hold: bool,
}

impl Reply {
    pub(crate) fn set_json(&mut self, value: Value) {
        self.body = value.to_string().into_bytes();
    }
    pub(crate) fn json(value: Value) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: value.to_string().into_bytes(),
            declared: None,
            chunked: false,
            hold: false,
        }
    }

    pub(crate) fn held() -> Self {
        Self {
            hold: true,
            ..Self::json(Value::Null)
        }
    }

    pub(crate) fn rate_limited() -> Self {
        Self {
            status: 429,
            ..Self::json(Value::Null)
        }
    }
}

pub(crate) struct Server {
    address: SocketAddr,
    certificate: reqwest::Certificate,
    stopped: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    pub(crate) fn new(route: impl Fn(&str) -> Reply + Send + 'static) -> Self {
        let certificate = rcgen::generate_simple_self_signed(vec![
            "api.github.com".into(),
            "raw.githubusercontent.com".into(),
            "release-assets.githubusercontent.com".into(),
            "objects.githubusercontent.com".into(),
        ])
        .unwrap();
        let root = reqwest::Certificate::from_der(certificate.cert.der()).unwrap();
        let key =
            rustls::pki_types::PrivateKeyDer::Pkcs8(certificate.signing_key.serialize_der().into());
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate.cert.der().clone()], key)
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let exit = stopped.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let worker = std::thread::spawn(move || {
            while !exit.load(Ordering::Acquire) {
                let (socket, _) = match listener.accept() {
                    Ok(accepted) => accepted,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("owned TLS listener failed: {error}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_millis(100)))
                    .unwrap();
                let mut stream = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(Arc::new(tls.clone())).unwrap(),
                    socket,
                );
                let deadline = Instant::now() + Duration::from_secs(3);
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n")
                    && request.len() < 8192
                    && Instant::now() < deadline
                    && !exit.load(Ordering::Acquire)
                {
                    let mut byte = [0];
                    match stream.read(&mut byte) {
                        Ok(0) => break,
                        Ok(_) => request.push(byte[0]),
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(_) => break,
                    }
                }
                if !request.ends_with(b"\r\n\r\n") {
                    continue;
                }
                let request = String::from_utf8(request).unwrap();
                captured.lock().unwrap().push(request.clone());
                let reply = route(&request);
                if reply.hold {
                    let until = Instant::now() + Duration::from_secs(15);
                    while !exit.load(Ordering::Acquire) && Instant::now() < until {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    continue;
                }
                let mut headers =
                    format!("HTTP/1.1 {} Fixture\r\nConnection: close\r\n", reply.status);
                for (name, value) in reply.headers {
                    headers.push_str(&format!("{name}: {value}\r\n"));
                }
                if reply.chunked {
                    headers.push_str("Transfer-Encoding: chunked\r\n");
                } else {
                    headers.push_str(&format!(
                        "Content-Length: {}\r\n",
                        reply.declared.unwrap_or(reply.body.len())
                    ));
                }
                headers.push_str("\r\n");
                let _ = stream.write_all(headers.as_bytes());
                if reply.chunked {
                    let _ = stream.write_all(format!("{:x}\r\n", reply.body.len()).as_bytes());
                    let _ = stream.write_all(&reply.body);
                    let _ = stream.write_all(b"\r\n0\r\n\r\n");
                } else {
                    let _ = stream.write_all(&reply.body);
                }
                let _ = stream.flush();
            }
        });
        Self {
            address,
            certificate: root,
            stopped,
            requests,
            worker: Some(worker),
        }
    }

    fn client(&self, trust: bool) -> Client {
        let builder = client_builder()
            .resolve("api.github.com", self.address)
            .resolve("raw.githubusercontent.com", self.address)
            .resolve("release-assets.githubusercontent.com", self.address)
            .resolve("objects.githubusercontent.com", self.address);
        let builder = if trust {
            builder.tls_certs_only([self.certificate.clone()])
        } else {
            builder
        };
        builder.build().unwrap()
    }

    pub(crate) fn job(&self, repository: Repository) -> Result<Job, Problem> {
        Job::start_with(repository, Some(self.client(true)))
    }

    pub(crate) fn refresh(&self, repository_id: u64) -> Result<Job, Problem> {
        Job::start_target(Target::Registered(repository_id), Some(self.client(true)))
    }

    pub(crate) fn install(
        &self,
        selection: Selection,
        root: PathBuf,
    ) -> Result<InstallJob, Problem> {
        InstallJob::start_with(selection, root, Some(self.client(true)))
    }

    pub(crate) fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    pub(crate) fn inspect(&self, trust: bool) -> Result<Inspection, Problem> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(
            Operation {
                client: self.client(trust),
                cancelled: Arc::new(AtomicBool::new(false)),
                deadline: Instant::now() + OPERATION_TIMEOUT,
            }
            .inspect(Repository::parse("example/sample").unwrap()),
        )
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

#[test]
fn actual_install_worker_failure_has_identity_generation_and_failed_terminal_progress_without_selection(
) {
    let _serial = JOB_TESTS.lock().unwrap();
    let root = super::super::package::tests::OwnedRoot::new();
    let (metadata, _) = super::super::package::tests::fixture();
    let server = Server::new(|_| Reply::rate_limited());
    let job = server.install(selected(&metadata), root.0.clone()).unwrap();
    assert_eq!(job.operation().0, "github:12/sample");
    assert_eq!(job.operation().1.len(), 64);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(outcome) = job.poll() {
            assert!(outcome.is_err());
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(job.phase(), "Package installation failed");
    assert!(!root.0.join("installed.json").exists());
    while WORKERS.load(Ordering::Acquire) != 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}

pub(crate) fn normal(request: &str) -> Reply {
    let path = request.split_whitespace().nth(1).unwrap();
    if path == "/repos/example/sample" || path == "/repositories/12" {
        Reply::json(
            serde_json::json!({"id":12,"owner":{"login":"example","id":34},"name":"sample","private":false,"default_branch":"main"}),
        )
    } else if path == "/repos/example/sample/commits/main"
        || path == "/repositories/12/commits/main"
    {
        Reply::json(serde_json::json!({"sha":"a".repeat(40)}))
    } else if path == format!("/example/sample/{}/among-plugins.json", "a".repeat(40)) {
        Reply::json(crate::plugin::install::metadata::tests::single())
    } else {
        panic!("unexpected fixture request path: {path}");
    }
}

#[test]
fn actual_https_requests_pin_the_catalogue_and_reject_hostile_transport_outcomes() {
    let server = Server::new(normal);
    let inspection = server.inspect(true).unwrap();
    assert_eq!(inspection.repository_id, 12);
    assert_eq!(inspection.catalogue_revision, "a".repeat(40));
    assert_eq!(inspection.catalogue.plugins[0].id, "sample");
    assert!(inspection
        .summary(|| Ok(()))
        .unwrap()
        .contains("inspection does not install or enable plugins"));
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].starts_with(&format!(
        "GET /example/sample/{}/among-plugins.json ",
        "a".repeat(40)
    )));
    assert!(requests
        .iter()
        .all(|request| !request.to_lowercase().contains("authorization:")
            && !request.to_lowercase().contains("cookie:")));
    drop(server);

    let server = Server::new(|request| {
        if request.starts_with("GET /repos/example/sample ") {
            Reply {
                status: 301,
                headers: vec![(
                    "Location".into(),
                    "https://api.github.com/repositories/12".into(),
                )],
                ..Reply::json(Value::Null)
            }
        } else {
            normal(request)
        }
    });
    assert_eq!(server.inspect(true).unwrap().repository_id, 12);
    assert_eq!(server.requests().len(), 4);
    drop(server);

    let server = Server::new(|request| {
        if request.starts_with("GET /repos/example/sample ") {
            Reply {
                status: 301,
                headers: vec![(
                    "Location".into(),
                    "https://api.github.com/repositories/99".into(),
                )],
                ..Reply::json(Value::Null)
            }
        } else {
            Reply::json(
                serde_json::json!({"id":12,"owner":{"login":"example","id":34},"name":"sample","private":false,"default_branch":"main"}),
            )
        }
    });
    assert_eq!(
        server.inspect(true).err().unwrap().0,
        "repository redirect and response identities disagree"
    );
    assert_eq!(server.requests().len(), 2);
    drop(server);

    for location in [
        "https://unapproved.example/steal?token=synthetic",
        "http://api.github.com/repositories/12",
        "https://user:synthetic@api.github.com/repositories/12",
        "https://api.github.com/repos/another/origin",
        "https://api.github.com/repositories/12?token=synthetic",
    ] {
        let location = location.to_string();
        let server = Server::new(move |_| Reply {
            status: 302,
            headers: vec![("Location".into(), location.clone())],
            ..Reply::json(Value::Null)
        });
        assert_eq!(
            server.inspect(true).err().unwrap().0,
            "repository redirect changed the approved origin or resource"
        );
        assert_eq!(server.requests().len(), 1);
    }
    let server = Server::new(|_| Reply {
        status: 302,
        headers: vec![(
            "Location".into(),
            "https://api.github.com/repositories/99".into(),
        )],
        ..Reply::json(Value::Null)
    });
    assert_eq!(
        server.inspect(true).err().unwrap().0,
        "repository redirect budget exceeded"
    );
    assert_eq!(server.requests().len(), MAX_REDIRECTS + 1);
    drop(server);

    let server = Server::new(|_| Reply {
        status: 429,
        ..Reply::json(Value::Null)
    });
    assert!(server.inspect(true).err().unwrap().0.contains("rate limit"));
    assert_eq!(server.requests().len(), 1);
    drop(server);
    let server = Server::new(|_| Reply {
        declared: Some(data::MAX_STATIC_BYTES + 1),
        ..Reply::json(Value::Null)
    });
    assert_eq!(
        server.inspect(true).err().unwrap().0,
        "repository response exceeds its byte budget"
    );
    drop(server);
    let server = Server::new(|_| Reply {
        body: vec![b' '; data::MAX_STATIC_BYTES + 1],
        chunked: true,
        ..Reply::json(Value::Null)
    });
    assert_eq!(
        server.inspect(true).err().unwrap().0,
        "repository response exceeds its byte budget"
    );
    drop(server);
    let server = Server::new(|_| Reply {
        declared: Some(100),
        ..Reply::json(Value::Null)
    });
    assert_eq!(
        server.inspect(true).err().unwrap().0,
        "repository response was interrupted or invalid"
    );
    drop(server);
    let server = Server::new(normal);
    assert_eq!(
        server.inspect(false).err().unwrap().0,
        "HTTPS connection or certificate verification failed"
    );
    assert!(server.requests().is_empty());
}

#[test]
fn owned_workers_cancel_real_pending_https_without_exceeding_two_credits() {
    let _serial = JOB_TESTS.lock().unwrap();
    let server = Server::new(|_| Reply::held());
    let first = server
        .job(Repository::parse("example/sample").unwrap())
        .unwrap();
    let second = server
        .job(Repository::parse("example/sample").unwrap())
        .unwrap();
    assert!(
        matches!(server.job(Repository::parse("example/sample").unwrap()), Err(Problem(reason)) if reason.contains("two repository operations"))
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.requests().is_empty() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let start = Instant::now();
    first.cancel();
    second.cancel();
    for job in [&first, &second] {
        loop {
            if let Some(outcome) = job.poll() {
                assert_eq!(outcome.err().unwrap().0, "repository inspection cancelled");
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(1));
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    while WORKERS.load(Ordering::Acquire) != 0 {
        assert!(start.elapsed() < Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn selected(metadata: &Value) -> Selection {
    Inspection::from_bytes(
        Origin {
            repository_id: 12,
            publisher_id: 34,
            owner: "example".into(),
            repository: "sample".into(),
        },
        "a".repeat(40),
        metadata.to_string().into_bytes(),
    )
    .unwrap()
    .select("sample", "1.0")
    .unwrap()
}

pub(crate) fn package_route(metadata: Value, archive: Vec<u8>, request: &str) -> Reply {
    let path = request.split_whitespace().nth(1).unwrap();
    match path {
        "/repositories/12/releases/7" => Reply::json(serde_json::json!({"id":7,"draft":false,"assets":[
            {"id":11,"size":archive.len(),"state":"uploaded","digest":format!("sha256:{}", metadata["artifacts"][0]["sha256"].as_str().unwrap())}]})),
        "/repositories/12/releases/assets/11" => Reply { status: 302, headers: vec![("Location".into(),
            "https://release-assets.githubusercontent.com/github-production-release-asset/12/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee?token=synthetic-delivery-token".into())], ..Reply::json(Value::Null) },
        path if path.starts_with("/github-production-release-asset/12/") => Reply { body: archive, ..Reply::json(Value::Null) },
        path if path.ends_with("/among-plugins.json") => Reply::json(metadata),
        _ => normal(request),
    }
}

pub(crate) fn install_at(
    server: &Server,
    selected: Selection,
    root: &std::path::Path,
) -> Result<store::Commit, Problem> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(
        Operation {
            client: server.client(true),
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline: Instant::now() + OPERATION_TIMEOUT,
        }
        .install(selected, root, &AtomicU8::new(DOWNLOADING)),
    )
}

#[test]
fn real_update_transfer_requires_renewed_execution_review_and_schema_migrations_fail_before_download(
) {
    use crate::plugin::host::{PluginHost, PluginState};
    use crate::plugin::inventory;
    use serde_json::json;
    const KEY: &str = "github:12/sample";
    let root = super::super::package::tests::OwnedRoot::new();
    let (metadata, archive) =
        super::super::package::tests::managed_fixture("1.0", "managed-resource", |_| {});
    let server =
        Server::new(move |request| package_route(metadata.clone(), archive.clone(), request));
    let initial = install_at(
        &server,
        server
            .inspect(true)
            .unwrap()
            .select("sample", "1.0")
            .unwrap(),
        &root.0,
    )
    .unwrap()
    .receipt;
    let entry = inventory::read(&root.0).unwrap().remove(0);
    let local = json!({"enabled":true,"installation":initial.installation,"review":initial.review(vec!["status".into(),"settings".into()]),"settings":{"version":1,"values":{"label":"retained"}}});
    let mut host = PluginHost::new();
    host.register_managed(entry.clone(), Some(&local));
    let wait = |host: &mut PluginHost, predicate: &dyn Fn(&PluginHost) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            host.poll();
            if predicate(host) {
                break;
            }
            assert!(Instant::now() < deadline, "{:?}", host.listing());
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    wait(&mut host, &|host| host.running_generation(KEY).is_some());
    let (metadata, archive) =
        super::super::package::tests::managed_fixture("2.0", "managed-resource", |metadata| {
            metadata["manifest"]["settings"]["version"] = json!(2)
        });
    let migration =
        Server::new(move |request| package_route(metadata.clone(), archive.clone(), request));
    let selected = migration
        .inspect(true)
        .unwrap()
        .select("sample", "2.0")
        .unwrap();
    assert!(selected.for_update(&entry, local.get("settings")).is_err());
    assert_eq!(migration.requests().len(), 3);
    assert_eq!(inventory::read(&root.0).unwrap()[0].receipt, initial);
    let (metadata, archive) =
        super::super::package::tests::managed_fixture("2.0", "managed-resource", |metadata| {
            metadata["manifest"]["required_features"] =
                json!(["status", "settings", "context.terminals", "context.title"])
        });
    let transferred = Server::new(move |request| {
        if request.starts_with("GET /repos/example/sample ") {
            Reply::json(
                json!({"id":12,"owner":{"login":"example","id":99},"name":"sample","private":false,"default_branch":"main"}),
            )
        } else {
            package_route(metadata.clone(), archive.clone(), request)
        }
    });
    let selected = transferred
        .inspect(true)
        .unwrap()
        .select("sample", "2.0")
        .unwrap()
        .for_update(&entry, local.get("settings"))
        .unwrap();
    assert!(selected.review_description().contains("Publisher changed"));
    let outcome = install_at(&transferred, selected, &root.0).unwrap();
    let current = inventory::read(&root.0).unwrap().remove(0);
    host.reconcile_managed(current.clone(), Some(&local));
    wait(
        &mut host,
        &|host| matches!(&host.listing()[0].1,PluginState::Unavailable{reason} if reason.contains("publisher")),
    );
    assert!(!outcome
        .receipt
        .package_path(&root.0)
        .join("executed")
        .exists());
    assert!(initial.package_path(&root.0).join("executed").exists());
    host.disable(KEY);
    host.review_access(
        KEY,
        outcome.receipt.review(vec![
            "status".into(),
            "settings".into(),
            "context.terminals".into(),
            "context.title".into(),
        ]),
    )
    .unwrap();
    host.enable(KEY);
    wait(&mut host, &|host| host.running_generation(KEY).is_some());
    assert!(outcome
        .receipt
        .package_path(&root.0)
        .join("executed")
        .exists());
    host.shutdown();
    wait(&mut host, &|host| host.is_stopped());
    println!("PASS actual old/new managed processes, transfer plus expanded required access refused before execution until renewed numeric review; unsupported schema migration refused before asset request");
}

#[test]
fn actual_https_stream_commits_exact_selection_inert_and_loads_offline() {
    let root = crate::plugin::install::package::tests::OwnedRoot::new();
    let (metadata, archive) = crate::plugin::install::package::tests::fixture();
    let selection = selected(&metadata);
    let catalogue_digest =
        crate::plugin::inventory::checksum(metadata.to_string().as_bytes()).unwrap();
    let server =
        Server::new(move |request| package_route(metadata.clone(), archive.clone(), request));
    let outcome = install_at(&server, selection, &root.0).unwrap();
    assert_eq!(outcome.durability, crate::settings::Durability::Durable);
    assert_eq!(outcome.receipt.revision, "a".repeat(40));
    assert_eq!(outcome.receipt.release_id, 7);
    assert_eq!(outcome.receipt.asset_id, 11);
    assert_eq!(outcome.receipt.source.as_ref().unwrap().publisher_id, 34);
    assert_eq!(
        outcome.receipt.source.as_ref().unwrap().catalogue_sha256,
        catalogue_digest
    );
    assert_eq!(
        outcome.receipt.source.as_ref().unwrap().catalogue_revision,
        "a".repeat(40)
    );
    assert!(outcome.receipt.installation.is_some());
    let requests = server.requests();
    assert_eq!(
        requests.len(),
        3,
        "install must not refetch mutable latest/catalogue"
    );
    assert!(requests[0].starts_with("GET /repositories/12/releases/7 "));
    assert!(requests[1].starts_with("GET /repositories/12/releases/assets/11 "));
    assert!(requests
        .iter()
        .all(|request| !request.to_lowercase().contains("authorization:")
            && !request.to_lowercase().contains("cookie:")
            && !request.to_lowercase().contains("referer:")));
    drop(server);
    let entries = crate::plugin::inventory::read(&root.0).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].inspected.is_ok());
    let mut host = crate::plugin::host::PluginHost::new();
    host.register_managed(entries[0].clone(), None);
    assert!(!host.desired_enabled("github:12/sample"));
    assert_eq!(host.supervised_count(), 0);
}

#[test]
fn actual_network_release_hash_transfer_and_redirect_failures_never_select_files() {
    for failure in [
        "release",
        "asset",
        "size",
        "hash",
        "corrupted-body",
        "truncated",
        "oversized",
        "cross-repository",
        "credentials",
        "http",
        "host",
    ] {
        let root = crate::plugin::install::package::tests::OwnedRoot::new();
        let (metadata, archive) = crate::plugin::install::package::tests::fixture();
        let selection = selected(&metadata);
        let server = Server::new(move |request| {
            let mut reply = package_route(metadata.clone(), archive.clone(), request);
            if request.starts_with("GET /repositories/12/releases/7 ") {
                let mut value: Value = serde_json::from_slice(&reply.body).unwrap();
                match failure {
                    "release" => value["id"] = Value::from(8),
                    "asset" => value["assets"][0]["id"] = Value::from(99),
                    "size" => value["assets"][0]["size"] = Value::from(1),
                    "hash" => {
                        value["assets"][0]["digest"] =
                            Value::from(format!("sha256:{}", "0".repeat(64)))
                    }
                    _ => {}
                }
                reply = Reply::json(value);
            } else if request.starts_with("GET /repositories/12/releases/assets/11 ") {
                let location = match failure {
                    "cross-repository" => Some("https://release-assets.githubusercontent.com/github-production-release-asset/99/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee?token=synthetic"),
                    "credentials" => Some("https://user:synthetic@release-assets.githubusercontent.com/github-production-release-asset/12/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
                    "http" => Some("http://release-assets.githubusercontent.com/github-production-release-asset/12/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
                    "host" => Some("https://unapproved.example/github-production-release-asset/12/aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee?token=synthetic"),
                    _ => None,
                };
                if let Some(location) = location {
                    reply.headers[0].1 = location.into();
                }
            } else if request.contains("/github-production-release-asset/12/") {
                match failure {
                    "corrupted-body" => {
                        reply.body.fill(0);
                    }
                    "truncated" => {
                        reply.declared = Some(reply.body.len());
                        reply.body.truncate(reply.body.len() / 2);
                    }
                    "oversized" => {
                        reply.body.push(0);
                        reply.chunked = true;
                    }
                    _ => {}
                }
            }
            reply
        });
        let error = install_at(&server, selection, &root.0).unwrap_err();
        assert!(
            !error.0.contains("synthetic"),
            "diagnostics must omit signed URLs"
        );
        assert!(!root.0.join("installed.json").exists(), "{failure}");
        assert!(crate::plugin::inventory::read(&root.0).unwrap().is_empty());
    }
}

#[test]
fn cancellation_of_actual_package_worker_cannot_report_a_committed_rollback() {
    let _serial = JOB_TESTS.lock().unwrap();
    let root = crate::plugin::install::package::tests::OwnedRoot::new();
    let (metadata, archive) = crate::plugin::install::package::tests::fixture();
    let selection = selected(&metadata);
    let server = Server::new(move |request| {
        if request.starts_with("GET /repositories/12/releases/assets/11 ") {
            Reply::held()
        } else {
            package_route(metadata.clone(), archive.clone(), request)
        }
    });
    let job = server.install(selection, root.0.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.requests().len() < 2 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(job.cancel());
    loop {
        if let Some(outcome) = job.poll() {
            assert!(outcome.is_err());
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!root.0.join("installed.json").exists());
    drop(server);
    let root = crate::plugin::install::package::tests::OwnedRoot::new();
    let (metadata, archive) = crate::plugin::install::package::tests::fixture();
    let selection = selected(&metadata);
    let server =
        Server::new(move |request| package_route(metadata.clone(), archive.clone(), request));
    let job = server.install(selection, root.0.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.0.join("installed.json").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !job.cancel(),
        "a selected package cannot be cancelled as though rolled back"
    );
    loop {
        if let Some(outcome) = job.poll() {
            assert!(outcome.is_ok());
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(crate::plugin::inventory::read(&root.0).unwrap().len(), 1);
}

#[test]
fn a_real_silent_https_peer_hits_the_unchanged_ten_second_idle_limit() {
    let server = Server::new(|_| Reply::held());
    let start = Instant::now();
    assert_eq!(
        server.inspect(true).err().unwrap().0,
        "repository request timed out"
    );
    assert!(start.elapsed() >= READ_TIMEOUT - Duration::from_millis(250));
    assert!(start.elapsed() < READ_TIMEOUT + Duration::from_secs(5));
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn source_inputs_do_not_accept_credentials_cross_origins_or_reference_paths() {
    for input in [
        "https://github.com/owner/repo?token=synthetic",
        "https://user:synthetic@github.com/owner/repo",
        "https://unapproved.example/owner/repo",
        "http://github.com/owner/repo",
        "https://github.com:444/owner/repo",
        "https://github.com/owner/repo/tree/main",
        "owner/../repo",
        "owner/repo#fragment",
    ] {
        assert!(Repository::parse(input).is_err(), "{input}");
    }
    assert_eq!(
        Repository::parse("owner/repo").unwrap(),
        Repository::parse("https://github.com/owner/repo/").unwrap()
    );
}

#[test]
fn actual_numeric_refresh_refuses_deleted_recreated_names_without_retargeting_saved_identity() {
    let _serial = JOB_TESTS.lock().unwrap();
    use super::super::registry::{self, Outcome, Request};
    use serde_json::json;
    let root = super::super::package::tests::OwnedRoot::new();
    let recreated = Arc::new(AtomicBool::new(false));
    let server = Server::new({
        let recreated = recreated.clone();
        move |request| {
            let path = request.split_whitespace().nth(1).unwrap();
            if path == "/repos/example/sample" || path == "/repositories/12" {
                Reply::json(
                    json!({"id":if recreated.load(Ordering::Acquire){55}else{12},"owner":{"login":"example","id":34},"name":"sample","private":false,"default_branch":"main"}),
                )
            } else if path == "/repositories/55/commits/main" {
                Reply::json(json!({"sha":"a".repeat(40)}))
            } else {
                normal(request)
            }
        }
    });
    let first = server.inspect(true).unwrap();
    registry::run(&root.0, Request::Add(Box::new(first)), || Ok(()), || Ok(())).unwrap();
    let expected = match registry::run(&root.0, Request::List, || Ok(()), || Ok(())).unwrap() {
        Outcome::List(mut entries) => entries.remove(0),
        _ => panic!("expected registrations"),
    };
    let before = std::fs::read(root.0.join("sources.json")).unwrap();
    recreated.store(true, Ordering::Release);
    let job = server.refresh(12).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(outcome) = job.poll() {
            assert!(matches!(
                outcome,
                Err(Problem(
                    "registered numeric repository identity changed; refusing to retarget source"
                ))
            ));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(std::fs::read(root.0.join("sources.json")).unwrap(), before);
    let new = server.inspect(true).unwrap();
    assert_eq!(
        new.origin().compare(&expected.origin),
        crate::plugin::origin::Change::IdentityChanged
    );
    assert!(registry::run(
        &root.0,
        Request::Refresh {
            expected,
            inspection: Box::new(new.clone())
        },
        || Ok(()),
        || Ok(())
    )
    .is_err());
    registry::run(&root.0, Request::Add(Box::new(new)), || Ok(()), || Ok(())).unwrap();
    match registry::run(&root.0, Request::List, || Ok(()), || Ok(())).unwrap() {
        Outcome::List(entries) => assert_eq!(
            entries
                .iter()
                .map(|entry| entry.origin.repository_id)
                .collect::<Vec<_>>(),
            vec![12, 55]
        ),
        _ => panic!("expected separate sources"),
    };
}

#[test]
fn actual_network_refresh_finishing_after_committed_source_removal_cannot_resurrect_registration() {
    let _serial = JOB_TESTS.lock().unwrap();
    use super::super::registry::{self, Outcome, Request};
    let root = super::super::package::tests::OwnedRoot::new();
    let release = Arc::new(AtomicBool::new(true));
    let server = Server::new({
        let release = release.clone();
        move |request| {
            if request
                .split_whitespace()
                .nth(1)
                .unwrap()
                .ends_with("/among-plugins.json")
            {
                let deadline = Instant::now() + Duration::from_secs(10);
                while !release.load(Ordering::Acquire) {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            normal(request)
        }
    });
    let initial = server.inspect(true).unwrap();
    registry::run(
        &root.0,
        Request::Add(Box::new(initial)),
        || Ok(()),
        || Ok(()),
    )
    .unwrap();
    let expected = match registry::run(&root.0, Request::List, || Ok(()), || Ok(())).unwrap() {
        Outcome::List(mut entries) => entries.remove(0),
        _ => panic!("expected source"),
    };
    release.store(false, Ordering::Release);
    let network = server.refresh(12).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.requests().len() < 6 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let removal = registry::Job::start(root.0.clone(), Request::Remove(expected.clone())).unwrap();
    loop {
        if let Some(outcome) = removal.poll() {
            assert!(matches!(outcome, Ok(Outcome::Changed { .. })));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let removed = std::fs::read(root.0.join("sources.json")).unwrap();
    release.store(true, Ordering::Release);
    let inspection = loop {
        if let Some(outcome) = network.poll() {
            break outcome.unwrap();
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    };
    let late = registry::Job::start(
        root.0.clone(),
        Request::Refresh {
            expected,
            inspection: Box::new(inspection),
        },
    )
    .unwrap();
    loop {
        if let Some(outcome) = late.poll() {
            assert!(matches!(
                outcome,
                Err(Problem(
                    "source was removed or changed; stale operation was refused"
                ))
            ));
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(std::fs::read(root.0.join("sources.json")).unwrap(), removed);
    assert!(
        matches!(registry::run(&root.0,Request::List,||Ok(()),||Ok(())).unwrap(),Outcome::List(entries) if entries.is_empty())
    );
}

#[test]
fn real_install_workers_serialize_identity_and_use_fresh_operation_generations_after_cancellation()
{
    let _serial = JOB_TESTS.lock().unwrap();
    let root = super::super::package::tests::OwnedRoot::new();
    let (metadata, archive) = super::super::package::tests::fixture();
    let selection = selected(&metadata);
    let server = Server::new(|_| Reply::held());
    let first = server.install(selection.clone(), root.0.clone()).unwrap();
    let generation = first.operation().1.to_string();
    assert_eq!(first.operation().0, "github:12/sample");
    assert!(
        matches!(server.install(selection.clone(),root.0.clone()),Err(Problem(reason)) if reason.contains("identity still has an active"))
    );
    assert!(first.cancel());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(outcome) = first.poll() {
            assert!(outcome.is_err());
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    while WORKERS.load(Ordering::Acquire) != 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(first.phase(), "Installation cancelled before commitment");
    drop(server);
    assert!(!root.0.join("installed.json").exists());
    let server =
        Server::new(move |request| package_route(metadata.clone(), archive.clone(), request));
    let next = server.install(selection, root.0.clone()).unwrap();
    assert_ne!(next.operation().1, generation);
    loop {
        if let Some(outcome) = next.poll() {
            assert!(outcome.is_ok());
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        !next.cancel(),
        "a committed operation must never report a cancelled rollback"
    );
    assert_eq!(crate::plugin::inventory::read(&root.0).unwrap().len(), 1);
}
