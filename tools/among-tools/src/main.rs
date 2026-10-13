use among_tools::{display, ignored_tests, inventory, platform, preferences, resources, Result};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const HELP: &str = "among-tools — independent headless project tooling

Usage:
  among-tools inventory freeze <project-root> <new-inventory.json>
  among-tools inventory check <project-root> <inventory.json>
  among-tools platform discover
  among-tools platform check <os> <version> <architecture>
  among-tools ci-x11 <command> [args...]
  among-tools ci-x11-video <command> [args...]
  among-tools isolated-x11-video <command> [args...]
  among-tools isolated-x11 <command> [args...]
  among-tools vm-x11 <command> [args...]
  among-tools run-ignored-tests <cargo-test-executables.jsonl>
  among-tools check-resource-measurements <system-status-binary>
  among-tools check-preferences <prebuilt-app-test> <prebuilt-plugins-directory>
  among-tools check-preferences-from-cargo <cargo-test-executables.jsonl> <prebuilt-plugins-directory>
  among-tools install-prebuilt <source-root> <among-binary> <native-uninstaller>
  among-tools install <source-root> [--binary PATH | --install-deps | --uninstall]
  among-tools check-plugin-install <source-root> <among-binary> <prebuilt-plugins> <prebuilt-plugin-installer>
  among-tools check-sessions <among-binary>
  among-tools check-live-focus <among-binary>
  among-tools check-live-resize <among-binary>
  among-tools check-live-status <source-root> <among-binary> <prebuilt-plugins>
  among-tools check-desktop <source-root> <among-binary>
  among-tools check-context-menu-launch <among-binary>
  among-tools check-context-menu <among-binary> <prebuilt-clipboard-helper> <light|dark>
  among-tools check-context-menu-snap <prebuilt-clipboard-helper> <light|dark> [--github-hosted]
  among-tools check-snap-runtime [--github-hosted]
  among-tools deb-release-inputs <source-root> <stable-tag> <debian13|ubuntu24.04> <amd64|arm64>
  among-tools stage-deb <source-root> <binaries> <new-destination>
  among-tools deb-tag-source <source-root>
  among-tools check-deb-selection <source-root> <binaries>
  among-tools check-deb-source-inputs <source-root>
  among-tools source-snapshot <source-root> <vendor-directory> <new-output.tar>
  among-tools ci-deb-build <source-root> <target> <architecture> <digest-pinned-image> <new-output>
  among-tools check-deb-workflow <source-root>
  among-tools check-deb-desktop
  among-tools check-native-checker <directory> <source-sha> <tag> <target> <architecture>
  among-tools package-native-checker <binary> <source-sha> <tag> <target> <architecture> <new-directory>
  among-tools ci-deb-install <source-root> <target> <architecture> <candidate> <new-output>
  among-tools check-source-delivery <association.json> <bundle.tar> <artifact.snap>
  among-tools release-policy <repository> <tag> <main-ref> <assets...> [--previous JSON] [--architectures JSON]
  among-tools prepare-stable-build <source-root> <new-output> <tag>
  among-tools check-stable-build <tree>
  among-tools check-release-metadata <source-root>
  among-tools check-stable-source <repository> <record.json> <bundle.tar>
  among-tools third-party-notices <source-root> <destination> <generate|check>
  among-tools check-source-snapshot <archive.tar>
  among-tools check-snap-notices <native-inventory.json> <prime>
  among-tools select-nightly <repository-dir> <owner/repo> <new-output.json> [--now ISO]
  among-tools package-version <source-root>
  among-tools check-native-source-unpack <sources> <new-output>
  among-tools check-snap-sources <native-inventory.json> <sources>
  among-tools check-apt-provenance <metadata> <native-inventory.json> <sources>
  among-tools collect-apt-provenance <new-output>
  among-tools check-promotion-admission <record.json> <gates.json> <artifact-directory> [--approved]
  among-tools release-promotion <record.json> <gates.json> <artifact-directory> <owner/name> <source-root> [--lock=<path>]
  among-tools check-no-python <source-root>
  among-tools check-rpm-spec <source-root>
  among-tools prepare-rpm-release <source-root> <output.spec>
  among-tools stage-appimage <source-root> <binaries-directory> <launcher> <architecture> <new-AppDir>
  among-tools fetch-appimage-tools <architecture> <new-tools-directory>
  among-tools build-appimage <staged-AppDir> <tools-directory> <amd64|arm64> <version> <new-output>
  among-tools appimage-version <source-root> <ref-type> <ref-name>
  among-tools collect-appimage-sources <audit.json> <new-output>
  among-tools ci-appimage-install <source-root> <debian13|ubuntu24.04|fedora43> <amd64|arm64> <candidate-directory> <absolute-check-root> <new-output>
  among-tools record-rpm-candidate <source-root> <target> <architecture> <candidate-directory>
  among-tools ci-rpm-install <source-root> <target> <architecture> <candidate-directory> <new-output>
  among-tools check-rpm-notices <source-root> <extracted-payload> <requires.txt>
  among-tools check-rpm-source-archive <source-root> <tar-listing.txt> <top-directory>
  among-tools make-rpm-older-fixture <candidate.rpm> <empty-work-directory> <candidate-directory>
  among-tools review-readme-action <source-root> <new-private-evidence-directory>
  among-tools review-readme-promo <source-root> <video-basename.mp4> <new-private-evidence-directory>
  among-tools prepare-readme-promo <source-root> <new-private-workspace>
  among-tools render-readme-promo <source-root> <new-private-workspace>
  among-tools render-readme-action <source-root> <new-private-workspace>
  among-tools export-demo <private-capture-directory> <new-private-evidence-directory>
  among-tools assemble-demo <reviewed-clip-directory> <new-private-evidence-directory>
  among-tools check-demo-clips <private-capture-directory> <new-private-evidence-directory>
  among-tools encode-demo-clip <private-raw-video> <recorded-events.json> <new-private-evidence-directory>
  among-tools capture-readme-plugins <source-root> <application> <native-plugin-directory> <on|off> <new-private-evidence-directory>
  among-tools inventory-snap-native <prime> <exact-debs> <new-output>
  among-tools package-source-delivery <snap> <snapshot> <native> <sources> <metadata> <prime> <builder> <instructions> <new-output>
  among-tools --help

Inventory commands read source as inert data; they never execute legacy tools.
Unported operations are not exposed as successful command placeholders.";

fn run(args: &[String]) -> Result<std::process::ExitCode> {
    match args {
        [command, architecture, destination] if command == "fetch-appimage-tools" => {
            among_tools::appimage_tools::fetch(architecture, Path::new(destination))?;
        }
        [command, staged, tools, architecture, version, output] if command == "build-appimage" => {
            among_tools::appimage_build::build(
                Path::new(staged),
                Path::new(tools),
                architecture,
                version,
                Path::new(output),
            )?;
        }
        [command, root, ref_type, ref_name] if command == "appimage-version" => {
            println!(
                "{}",
                among_tools::appimage_build::candidate_version(
                    Path::new(root),
                    ref_type,
                    ref_name
                )?
            );
        }
        [command, audit, output] if command == "collect-appimage-sources" => {
            among_tools::appimage_sources::collect(Path::new(audit), Path::new(output))?;
        }
        [command, root, host, architecture, candidate, check_root, output]
            if command == "ci-appimage-install" =>
        {
            among_tools::appimage_installed::run(
                Path::new(root),
                host,
                architecture,
                Path::new(candidate),
                Path::new(check_root),
                Path::new(output),
                &std::env::vars().collect(),
            )?;
        }
        [command, root, binaries, launcher, architecture, destination]
            if command == "stage-appimage" =>
        {
            among_tools::appimage::stage(
                Path::new(root),
                Path::new(binaries),
                Path::new(launcher),
                architecture,
                Path::new(destination),
            )?;
        }
        [command, root, target, architecture, directory] if command == "record-rpm-candidate" => {
            among_tools::rpm_installed::record(
                Path::new(root),
                target,
                architecture,
                Path::new(directory),
            )?;
        }
        [command, root, target, architecture, directory, output] if command == "ci-rpm-install" => {
            among_tools::rpm_installed::run(
                Path::new(root),
                target,
                architecture,
                Path::new(directory),
                Path::new(output),
            )?;
        }
        [command, root, payload, requires] if command == "check-rpm-notices" => {
            let report = among_tools::rpm_payload::check_notices(
                Path::new(root),
                Path::new(payload),
                &std::fs::read_to_string(requires)?,
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        [command, root, listing, top] if command == "check-rpm-source-archive" => {
            let tracked = among_tools::rpm_payload::tracked_files(Path::new(root))?;
            let count = among_tools::rpm_payload::check_source_listing(
                &tracked,
                &std::fs::read_to_string(listing)?,
                top,
            )?;
            println!("PASS Source0 carries exactly the {count} files of the tagged commit");
        }
        [command, candidate, work, output] if command == "make-rpm-older-fixture" => {
            let fixture = among_tools::rpm_lifecycle::make_older_fixture(
                Path::new(candidate),
                Path::new(work),
                Path::new(output),
            )?;
            println!("{}", fixture.display());
        }
        [command, root, output] if command == "prepare-rpm-release" => {
            println!(
                "{}",
                among_tools::rpm::prepare_release(Path::new(root), Path::new(output))?
            );
        }
        [command, root, app, plugins, mode, output] if command == "capture-readme-plugins" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::plugin_capture::capture(
                    Path::new(root),
                    Path::new(app),
                    Path::new(plugins),
                    mode,
                    Path::new(output)
                )?)?
            );
        }
        [command, raw, events, output] if command == "encode-demo-clip" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::demo_clip_media::encode(
                    Path::new(raw),
                    Path::new(events),
                    Path::new(output)
                )?)?
            );
        }
        [command, source, output] if command == "check-demo-clips" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::demo_clips::review(
                    Path::new(source),
                    Path::new(output)
                )?)?
            );
        }
        [command, source, output] if command == "assemble-demo" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::demo_assembly::assemble(
                    Path::new(source),
                    Path::new(output)
                )?)?
            );
        }
        [command, source, output] if command == "export-demo" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::demo_export::export(
                    Path::new(source),
                    Path::new(output)
                )?)?
            );
        }
        [command, root, output] if command == "render-readme-action" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::promo_render::render_action(
                    Path::new(root),
                    Path::new(output)
                )?)?
            );
        }
        [command, root, output] if command == "render-readme-promo" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::promo_render::render(
                    Path::new(root),
                    Path::new(output)
                )?)?
            );
        }
        [command, root, output] if command == "prepare-readme-promo" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::promo_prepare::prepare(
                    Path::new(root),
                    Path::new(output)
                )?)?
            );
        }
        [command, root, filename, output] if command == "review-readme-promo" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::promo_review::review(
                    Path::new(root),
                    filename,
                    Path::new(output)
                )?)?
            );
        }
        [command, root, output] if command == "review-readme-action" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::action_review::review(
                    Path::new(root),
                    Path::new(output)
                )?)?
            );
        }
        [command, root] if command == "check-rpm-spec" => {
            let root = Path::new(root);
            let report = among_tools::rpm::check(root, &among_tools::rpm::spec_path(root))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        [command, root] if command == "check-no-python" => {
            let findings = among_tools::python_guard::scan(Path::new(root))?;
            if !findings.is_empty() {
                for finding in &findings {
                    eprintln!("{}:{}: {}", finding.path, finding.line, finding.reason);
                }
                return Err(format!("Python prohibition failed with {} findings; guard implementation is partial until full adversarial suite is complete",findings.len()).into());
            }
            println!(
                "No findings in current bounded scan; guard is not final migration acceptance"
            );
        }
        [command, record, gates, assets, rest @ ..] if command == "check-promotion-admission" => {
            let approved = match rest {
                [] => false,
                [flag] if flag == "--approved" => true,
                _ => return Err("unknown promotion admission option".into()),
            };
            if !approved {
                return Err("protected publication approval is required".into());
            }
            let record = among_tools::storage::parse_json(&std::fs::read(record)?)?;
            let gates = among_tools::storage::parse_json(&std::fs::read(gates)?)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::promotion::check(
                    &record,
                    &gates,
                    Path::new(assets),
                    approved
                )?)?
            );
        }
        [command, record, gates, assets, repository, source_root, rest @ ..]
            if command == "release-promotion" =>
        {
            let lock = match rest {
                [] => among_tools::scratch::root()?.join("among-publication.lock"),
                [flag] if flag.starts_with("--lock=") => PathBuf::from(&flag[7..]),
                _ => return Err("release-promotion takes only --lock=<path>".into()),
            };
            // Approval is an environment fact, not an argument: an argument can
            // be added by anyone editing the command, while a protected
            // variable is set where the protected job is configured. It is
            // checked first so an unapproved run is refused before it reads a
            // record, a gate or an artifact, and names its own cause.
            let approved = std::env::var("AMONG_PROTECTED_APPROVAL").as_deref() == Ok("yes");
            if !approved {
                return Err("protected publication approval is required".into());
            }
            let record = among_tools::storage::parse_json(&std::fs::read(record)?)?;
            let gates = among_tools::storage::parse_json(&std::fs::read(gates)?)?;
            let directory = Path::new(assets);
            let mut supplied = BTreeMap::new();
            for name in record["assets"]
                .as_object()
                .ok_or("promotion record has no assets")?
                .keys()
            {
                supplied.insert(name.clone(), directory.join(name));
            }
            let source_root = PathBuf::from(source_root);
            among_tools::promotion_publication::preflight(
                &record,
                &gates,
                &supplied,
                approved,
                &source_root,
            )?;
            let workdir = match lock.parent() {
                Some(parent) => parent.to_path_buf(),
                None => among_tools::scratch::root()?,
            };
            let workdir = workdir.as_path();
            std::fs::create_dir_all(workdir)?;
            let mut destinations =
                among_tools::promotion_publication::Publication::new(repository, workdir)?;
            println!(
                "{}",
                among_tools::promotion::promote(
                    &record,
                    &gates,
                    &supplied,
                    &mut destinations,
                    approved,
                    workdir,
                    lock.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("lock"),
                )?
            );
        }
        [command, rest @ ..]
            if matches!(
                command.as_str(),
                "ci-x11" | "ci-x11-video" | "isolated-x11" | "isolated-x11-video" | "vm-x11"
            ) =>
        {
            let mode = match command.as_str() {
                "ci-x11" => display::Mode::Ci,
                "ci-x11-video" => display::Mode::Video,
                "isolated-x11-video" => display::Mode::VideoGnome,
                "vm-x11" => display::Mode::Guest,
                _ => display::Mode::NestedGnome,
            };
            let status = display::run(
                mode,
                rest,
                &among_tools::scratch::root()?,
                std::time::Duration::from_secs(300),
            )?;
            let code = status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .unwrap_or(1);
            return Ok(std::process::ExitCode::from(code));
        }
        [help] if help == "--help" || help == "-h" => println!("{HELP}"),
        [] => println!("{HELP}"),
        [command, output] if command == "collect-apt-provenance" => {
            among_tools::apt_collect::run(Path::new(output))?;
        }
        [command, rest @ ..] if command == "check-snap-runtime" => {
            let approved = match rest {
                [] => false,
                [flag] if flag == "--github-hosted" => true,
                _ => return Err(
                    "unknown Snap runtime option; modified-VTE/debugger acceptance is not ported"
                        .into(),
                ),
            };
            among_tools::snap_runtime::run(approved, &among_tools::scratch::root()?)?;
        }
        [command, output] if command == "snap-host-probe" => {
            among_tools::snap_runtime::host_probe(Path::new(output))?;
        }
        [command] if command == "snap-external-plugin" => {
            among_tools::snap_runtime::external_plugin()?
        }
        [command, snap, snapshot, native, sources, metadata, prime, builder, instructions, output]
            if command == "package-source-delivery" =>
        {
            let inputs = among_tools::delivery_package::Inputs {
                snap: Path::new(snap),
                snapshot: Path::new(snapshot),
                native: Path::new(native),
                sources: Path::new(sources),
                metadata: Path::new(metadata),
                prime: Path::new(prime),
                builder: Path::new(builder),
                instructions: Path::new(instructions),
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::delivery_package::package(
                    &inputs,
                    Path::new(output),
                    &among_tools::scratch::root()?
                )?)?
            );
        }
        [command, prime, debs, output] if command == "inventory-snap-native" => {
            among_tools::native_inventory::inventory(
                Path::new(prime),
                Path::new(debs),
                Path::new(output),
                &among_tools::scratch::root()?,
            )?;
        }
        [command, metadata, native, sources] if command == "check-apt-provenance" => {
            let native = among_tools::storage::parse_json(&std::fs::read(native)?)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::apt_provenance::authenticate(
                    Path::new(metadata),
                    &native,
                    Path::new(sources),
                    &among_tools::scratch::root()?
                )?)?
            );
        }
        [command, native, sources] if command == "check-snap-sources" => {
            let native = among_tools::storage::parse_json(&std::fs::read(native)?)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::native_sources::check(
                    &native,
                    Path::new(sources)
                )?)?
            );
        }
        [command, sources, output] if command == "check-native-source-unpack" => {
            among_tools::native_unpack::run(Path::new(sources), Path::new(output))?;
        }
        [command, root] if command == "package-version" => {
            println!("{}", among_tools::package_version::read(Path::new(root))?);
        }
        [command, repo, repository, output] if command == "select-nightly" => {
            among_tools::nightly::run(
                Path::new(repo),
                repository,
                Path::new(output),
                None,
                &among_tools::scratch::root()?,
            )?;
        }
        [command, repo, repository, output, option, now]
            if command == "select-nightly" && option == "--now" =>
        {
            among_tools::nightly::run(
                Path::new(repo),
                repository,
                Path::new(output),
                Some(now),
                &among_tools::scratch::root()?,
            )?;
        }
        [command, native, prime] if command == "check-snap-notices" => {
            let document = among_tools::storage::parse_json(&std::fs::read(native)?)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::native_notices::check(
                    &document,
                    Path::new(prime)
                )?)?
            );
        }
        [command, archive] if command == "check-source-snapshot" => {
            let files = among_tools::source_snapshot::check(Path::new(archive))?;
            println!("PASS safe source snapshot inventory and {files} exact member digests; tag binding and complete vendor rebuild are separate checks");
        }
        [command, root, destination, mode] if command == "third-party-notices" => {
            let check = match mode.as_str() {
                "check" => true,
                "generate" => false,
                _ => return Err("notice operation must be generate or check".into()),
            };
            among_tools::notices::run(
                Path::new(root),
                Path::new(destination),
                check,
                &among_tools::scratch::root()?,
            )?;
        }
        [command, repo, record, bundle] if command == "check-stable-source" => {
            let record = among_tools::storage::parse_json(&std::fs::read(record)?)?;
            println!(
                "{}",
                serde_json::to_string(&among_tools::stable_source::check(
                    Path::new(repo),
                    &record,
                    Path::new(bundle)
                )?)?
            );
        }
        [command, root] if command == "check-release-metadata" => {
            among_tools::release_metadata::run(Path::new(root), &among_tools::scratch::root()?)?;
        }
        [command, root, output, tag] if command == "prepare-stable-build" => {
            println!(
                "{}",
                serde_json::to_string(&among_tools::stable_build::prepare(
                    Path::new(root),
                    Path::new(output),
                    tag
                )?)?
            );
        }
        [command, tree] if command == "check-stable-build" => {
            among_tools::stable_build::check(Path::new(tree))?;
            println!("PASS isolated stable overlay and original source hashes");
        }
        [command, repo, tag, main, rest @ ..] if command == "release-policy" => {
            let mut assets = Vec::new();
            let mut previous = None;
            let mut architectures = None;
            let mut args = rest.iter();
            while let Some(arg) = args.next() {
                if matches!(arg.as_str(), "--previous" | "--architectures") {
                    let path = args
                        .next()
                        .ok_or("release-policy option requires JSON path")?;
                    let data = among_tools::storage::parse_json(&std::fs::read(path)?)?;
                    let destination = if arg == "--previous" {
                        &mut previous
                    } else {
                        &mut architectures
                    };
                    if destination.replace(data).is_some() {
                        return Err("duplicate release policy option".into());
                    }
                } else if arg.starts_with('-') {
                    return Err("unknown release policy option".into());
                } else {
                    assets.push(std::path::PathBuf::from(arg));
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::release_policy::stable(
                    Path::new(repo),
                    tag,
                    main,
                    &assets,
                    previous.as_ref(),
                    architectures.as_ref()
                )?)?
            );
        }
        [command, association, bundle, snap] if command == "check-source-delivery" => {
            println!(
                "{}",
                serde_json::to_string(&among_tools::source_delivery::check(
                    Path::new(association),
                    Path::new(bundle),
                    Path::new(snap)
                )?)?
            );
        }
        [command, root, target, architecture, candidate, output] if command == "ci-deb-install" => {
            among_tools::deb_installed::run(
                Path::new(root),
                target,
                architecture,
                Path::new(candidate),
                Path::new(output),
                &std::env::vars().collect(),
            )?;
        }
        [command, binary, source, tag, target, architecture, output]
            if command == "package-native-checker" =>
        {
            println!(
                "{}",
                serde_json::to_string_pretty(&among_tools::native_checker::package(
                    Path::new(binary),
                    source,
                    tag,
                    target,
                    architecture,
                    Path::new(output)
                )?)?
            );
        }
        [command, directory, source, tag, target, architecture]
            if command == "check-native-checker" =>
        {
            among_tools::native_checker::validate(
                Path::new(directory),
                source,
                tag,
                target,
                architecture,
            )?;
            println!("PASS source/target/digest-bound native checker");
        }
        [command] if command == "check-deb-desktop" => {
            among_tools::desktop::installed(&among_tools::scratch::root()?)?;
        }
        [command, root] if command == "check-deb-workflow" => {
            among_tools::deb_workflow::run(Path::new(root), &among_tools::scratch::root()?)?;
        }
        [command, root, target, architecture, image, output] if command == "ci-deb-build" => {
            among_tools::deb_build::run(
                Path::new(root),
                target,
                architecture,
                image,
                Path::new(output),
                &std::env::vars().collect(),
            )?;
        }
        [command, root, vendor, output] if command == "source-snapshot" => {
            println!(
                "{}",
                serde_json::to_string(&among_tools::source_snapshot::snapshot(
                    Path::new(root),
                    Path::new(vendor),
                    Path::new(output)
                )?)?
            );
        }
        [command, root] if command == "check-deb-source-inputs" => {
            among_tools::deb::check_source_inputs(Path::new(root))?;
            println!("PASS locked Debian build inputs and Cargo installation guide identity");
        }
        [command, root, binaries] if command == "check-deb-selection" => {
            among_tools::deb_selection::run(
                Path::new(root),
                Path::new(binaries),
                &among_tools::scratch::root()?,
            )?;
        }
        [command, root] if command == "deb-tag-source" => {
            let (tag, source) =
                among_tools::deb::github_tag_source(Path::new(root), &std::env::vars().collect())?;
            println!("{}", serde_json::json!({"tag":tag,"source_sha":source}));
        }
        [command, root, tag, target, architecture] if command == "deb-release-inputs" => {
            println!(
                "{}",
                serde_json::to_string(&among_tools::deb::identity(
                    Path::new(root),
                    tag,
                    target,
                    architecture
                )?)?
            );
        }
        [command, root, binaries, destination] if command == "stage-deb" => {
            println!(
                "Staged {} package files in {}",
                among_tools::deb::stage(
                    Path::new(root),
                    Path::new(binaries),
                    Path::new(destination)
                )?,
                destination
            );
        }
        [command, clipboard, theme, rest @ ..] if command == "check-context-menu-snap" => {
            let approved = match rest {
                [] => false,
                [flag] if flag == "--github-hosted" => true,
                _ => return Err("unknown installed menu option".into()),
            };
            among_tools::menu_check::run_snap(
                Path::new(clipboard),
                theme,
                &among_tools::scratch::root()?,
                approved,
            )?;
        }
        [command, binary, clipboard, theme] if command == "check-context-menu" => {
            among_tools::menu_check::run(
                Path::new(binary),
                Path::new(clipboard),
                theme,
                &among_tools::scratch::root()?,
            )?;
        }
        [command, output, mode] if command == "raw-menu-pty" => {
            among_tools::raw_pty::run(Path::new(output), mode)?;
        }
        [command, binary] if command == "check-context-menu-launch" => {
            among_tools::menu_launch::run(Path::new(binary), &among_tools::scratch::root()?)?;
        }
        [command, output, arguments @ ..] if command == "record-menu-uri" => {
            among_tools::menu_launch::record(Path::new(output), arguments)?;
        }
        [command, runner_home, entry] if command == "desktop-session" => {
            among_tools::desktop::session(Path::new(runner_home), Path::new(entry))?;
        }
        [command, root, binary] if command == "check-desktop" => {
            among_tools::desktop::run(
                Path::new(root),
                Path::new(binary),
                &among_tools::scratch::root()?,
            )?;
        }
        [command, root, binary, plugins] if command == "check-live-status" => {
            among_tools::live_status::run(
                Path::new(root),
                Path::new(binary),
                Path::new(plugins),
                &among_tools::scratch::root()?,
            )?;
        }
        [command, binary] if command == "check-live-focus" => {
            among_tools::live_focus::run(Path::new(binary), &among_tools::scratch::root()?)?;
        }
        [command, binary] if command == "check-live-resize" => {
            among_tools::live_resize::run(Path::new(binary), &among_tools::scratch::root()?)?;
        }
        [command, binary] if command == "check-sessions" => {
            among_tools::sessions::run(Path::new(binary), &among_tools::scratch::root()?)?;
        }
        [command, root, app, built, installer] if command == "check-plugin-install" => {
            among_tools::plugin_install::run(
                Path::new(root),
                Path::new(app),
                Path::new(built),
                Path::new(installer),
                &among_tools::scratch::root()?,
            )?;
        }
        [command, root, rest @ ..] if command == "install" => {
            if matches!(rest,[help] if help == "--help" || help == "-h") {
                println!("Install/update Among Terminal as the current user.\nUsage: scripts/install.sh [--binary PATH | --install-deps | --uninstall]\nPrebuilt installation requires native among-tools and sibling among-uninstall; source builds require Cargo/Rust >= 1.98 and GTK4/libadwaita/VTE development libraries.");
            } else {
                among_tools::installation::install(Path::new(root), rest)?;
            }
        }
        [command, root, binary, uninstaller] if command == "install-prebuilt" => {
            let layout = among_tools::installation::Layout::discover()?;
            let payload = among_tools::installation::source_payload(
                Path::new(root),
                &Path::new(binary).canonicalize()?,
                &Path::new(uninstaller).canonicalize()?,
                &layout,
            )?;
            among_tools::installation::install_payload(layout, payload)?;
            println!(
                "Installed Among Terminal for the current user; settings and sessions preserved."
            );
        }
        [command, records, plugins] if command == "check-preferences-from-cargo" => {
            preferences::from_cargo(
                Path::new(records),
                Path::new(plugins),
                &among_tools::scratch::root()?,
            )?;
        }
        [command, executable, plugins] if command == "check-preferences" => {
            preferences::run(
                Path::new(executable),
                Path::new(plugins),
                &among_tools::scratch::root()?,
            )?;
        }
        [command, binary] if command == "check-resource-measurements" => {
            resources::run(Path::new(binary), &among_tools::scratch::root()?)?;
        }
        [command, records] if command == "run-ignored-tests" => {
            let count = ignored_tests::run(Path::new(records), &among_tools::scratch::root()?)?;
            println!("Executed {count} ignored tests, one owned display per test");
        }
        [family, operation] if family == "platform" && operation == "discover" => {
            println!("{}", serde_json::to_string_pretty(&platform::discover()?)?);
        }
        [family, operation, os, version, architecture]
            if family == "platform" && operation == "check" =>
        {
            let actual = platform::discover()?;
            platform::validate_target(&actual, os, version, architecture)?;
            println!("{}", serde_json::to_string_pretty(&actual)?);
        }
        [family, operation, root, output] if family == "inventory" => {
            let root = Path::new(root);
            let output = Path::new(output);
            match operation.as_str() {
                "freeze" => {
                    let inventory = inventory::discover(root)?;
                    inventory::freeze(output, &inventory)?;
                    println!(
                        "Frozen {} Python sources and {} direct command/dependency references",
                        inventory.sources.len(),
                        inventory.direct_references.len()
                    );
                }
                "check" => {
                    inventory::check(root, output)?;
                    println!("Inventory matches owned source bytes and discovered callers");
                }
                _ => return Err(format!("unsupported inventory operation: {operation}").into()),
            }
        }
        _ => return Err("invalid command; use among-tools --help".into()),
    }
    Ok(std::process::ExitCode::SUCCESS)
}

fn main() -> std::process::ExitCode {
    match run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("among-tools: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
