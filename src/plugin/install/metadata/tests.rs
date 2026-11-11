use super::*;
use serde_json::json;

fn version(id: &str) -> Value {
    json!({"revision":"a".repeat(40),"manifest_sha256":"b".repeat(64),
        "manifest":{"schema_version":1,"id":id,"version":"1.0.0","name":"Equal display name",
            "description":"Ready-built fixture","entrypoint":"bin/plugin","api":{"major":1,"minor":2},
            "settings":{"version":1,"apply":"live","fields":[{"key":"label","label":"Label","kind":"string","default":id}]}},
        "artifacts":[{"os":"linux","architecture":"amd64","runtime":{"kind":"native","libc":"glibc","minimum":"2.35"},
            "dependencies":[],"format":"tar.gz","asset":"sample-amd64.tar.gz","size":123,"sha256":"c".repeat(64),
            "members":[{"path":"among-plugin.json","size":1024,"sha256":"b".repeat(64),"executable":false},
                {"path":"bin/plugin","size":256,"sha256":"d".repeat(64),"executable":true}]}]})
}

pub(crate) fn single() -> Value {
    let mut value = version("sample");
    value["schema_version"] = json!(2);
    value
}

fn parse(value: &Value) -> Result<Catalogue, Problem> {
    Catalogue::parse(&serde_json::to_vec(value).unwrap())
}

#[test]
fn package_members_cannot_impersonate_reserved_store_receipts_or_leases() {
    for path in [
        ".among-receipt.json",
        ".among-lease",
        ".among-lease/child",
        "data/.among-future",
    ] {
        let mut metadata = single();
        metadata["artifacts"][0]["members"]
            .as_array_mut()
            .unwrap()
            .push(json!({"path":path,"size":1,"sha256":"a".repeat(64),"executable":false}));
        assert!(parse(&metadata).is_err(), "{path}");
    }
}

#[test]
fn single_root_normalizes_and_shared_settings_remain_independent_in_a_catalogue() {
    let single = parse(&single()).unwrap();
    assert_eq!(single.plugins.len(), 1);
    assert_eq!(single.plugins[0].id, "sample");
    assert_eq!(single.plugins[0].versions.len(), 1);
    let document = json!({"schema_version":2,"plugins":[
        {"id":"first","versions":[version("first")]},{"id":"second","versions":[version("second")]}]});
    let multiple = parse(&document).unwrap();
    assert_eq!(multiple.plugins.len(), 2);
    for (index, id) in ["first", "second"].into_iter().enumerate() {
        let selected = &multiple.plugins[index].versions[0];
        assert_eq!(selected.manifest.id, id);
        let settings = selected
            .manifest
            .settings
            .as_ref()
            .unwrap()
            .effective(None)
            .unwrap();
        assert_eq!(settings["label"], id);
        assert_eq!(selected.revision, "a".repeat(40));
        assert_eq!(selected.manifest_sha256, "b".repeat(64));
        assert_eq!(selected.artifacts[0].asset, "sample-amd64.tar.gz");
        assert_eq!(selected.artifacts[0].architecture, Architecture::Amd64);
    }
}

#[test]
fn duplicate_keys_ids_versions_and_unknown_required_behaviour_are_refused() {
    assert!(Catalogue::parse(br#"{"schema_version":2,"schema_version":2,"plugins":[]}"#).is_err());
    let duplicate = json!({"schema_version":2,"plugins":[
        {"id":"sample","versions":[version("sample")]},{"id":"sample","versions":[version("sample")]}]});
    assert!(parse(&duplicate).is_err());
    assert!(parse(&json!({"schema_version":2,"plugins":[{"id":"sample","versions":[version("sample"),version("sample")]}]})).is_err());
    for field in [
        "post_install",
        "required_features",
        "repository",
        "download_url",
    ] {
        let mut value = single();
        value[field] = json!("never evaluate");
        assert!(parse(&value).is_err(), "{field}");
    }
    let mut value = single();
    value["schema_version"] = json!(1);
    assert!(parse(&value).is_err());
    let mut value = single();
    value["manifest"]["schema_version"] = json!(2);
    assert!(parse(&value).is_err());
    let mut value = single();
    value["manifest"]["settings"]["fields"][0]["validator"] =
        json!("https://invalid.example/never");
    assert!(parse(&value).is_err());
    let mismatch =
        json!({"schema_version":2,"plugins":[{"id":"other","versions":[version("sample")]}]});
    assert!(parse(&mismatch).is_err());
}

#[test]
fn exact_revisions_assets_sizes_formats_and_member_paths_are_required() {
    for revision in [
        "latest",
        "main",
        "a/b",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        let mut value = single();
        value["revision"] = json!(revision);
        assert!(parse(&value).is_err());
    }
    for path in [
        "../plugin",
        "/plugin",
        "bin/../plugin",
        "bin//plugin",
        "bin/./plugin",
    ] {
        let mut value = single();
        value["artifacts"][0]["members"][1]["path"] = json!(path);
        assert!(parse(&value).is_err());
    }
    for (key, bad) in [
        ("os", json!("other")),
        ("format", json!("deb")),
        ("asset", json!("../package.tar.gz")),
        ("size", json!(MAX_PACKAGE_BYTES + 1)),
        ("sha256", json!("wrong")),
        ("architecture", json!("amd64-or-arm64")),
    ] {
        let mut value = single();
        value["artifacts"][0][key] = bad;
        assert!(parse(&value).is_err(), "{key}");
    }
    let mut value = single();
    value["artifacts"][0]["members"][0]["sha256"] = json!("e".repeat(64));
    assert!(parse(&value).is_err());
    let mut value = single();
    value["artifacts"][0]["members"][1]["executable"] = json!(false);
    assert!(parse(&value).is_err());
    let mut value = single();
    let artifact = value["artifacts"][0].clone();
    value["artifacts"].as_array_mut().unwrap().push(artifact);
    assert!(parse(&value).is_err());
    let mut value = single();
    let file = value["artifacts"][0]["members"][0].clone();
    value["artifacts"][0]["members"]
        .as_array_mut()
        .unwrap()
        .push(file);
    assert!(parse(&value).is_err());
    let mut value = single();
    value["artifacts"][0]["members"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path":"bin","size":1,"sha256":"f".repeat(64),"executable":false}));
    assert!(parse(&value).is_err());
}

#[test]
fn schema_two_refuses_schema_one_and_embedded_api_authority_in_both_envelopes() {
    for schema in [0, 1, 3] {
        let mut value = single();
        value["schema_version"] = json!(schema);
        assert!(parse(&value).is_err());
        assert!(parse(&json!({"schema_version":schema,"plugins":[{"id":"sample","versions":[version("sample")]}]})).is_err());
    }
    for key in ["release_id", "asset_id", "download_url"] {
        let mut value = single();
        value[key] = json!(7);
        assert!(parse(&value).is_err(), "root {key}");
        let mut entry = version("sample");
        entry[key] = json!(7);
        assert!(
            parse(&json!({"schema_version":2,"plugins":[{"id":"sample","versions":[entry]}]}))
                .is_err(),
            "version {key}"
        );
        let mut value = single();
        value["artifacts"][0][key] = json!(11);
        assert!(parse(&value).is_err(), "artifact {key}");
    }
    for versions in [
        vec![],
        vec![version("sample"), {
            let mut different = version("sample");
            different["manifest"]["version"] = json!("2.0.0");
            different
        }],
    ] {
        assert!(parse(
            &json!({"schema_version":2,"plugins":[{"id":"sample","versions":versions}]})
        )
        .is_err());
    }
    for name in [
        "",
        ".",
        "..",
        "dir/package",
        "dir\\package",
        "a\nb",
        "among-plugins.json",
    ] {
        let mut value = single();
        value["artifacts"][0]["asset"] = json!(name);
        assert!(parse(&value).is_err(), "{name:?}");
    }
    let mut value = single();
    value["artifacts"][0]["asset"] = json!("x".repeat(256));
    assert!(parse(&value).is_err());
    value["artifacts"][0]["asset"] = json!("sample amd64+λ.tar.gz");
    assert_eq!(
        parse(&value).unwrap().plugins[0].versions[0].artifacts[0].asset,
        "sample amd64+λ.tar.gz"
    );
}

#[test]
fn catalogue_and_nested_object_counts_are_bounded_before_acceptance() {
    let mut value = single();
    value["padding"] = json!("x".repeat(MAX_CATALOGUE_BYTES));
    assert!(parse(&value).is_err());
    let plugins = (0..=MAX_PLUGINS)
        .map(|index| {
            let id = format!("plugin-{index}");
            json!({"id":id,"versions":[version(&id)]})
        })
        .collect::<Vec<_>>();
    assert!(parse(&json!({"schema_version":2,"plugins":plugins})).is_err());
    let versions = (0..2)
        .map(|index| {
            let mut version = version("sample");
            version["manifest"]["version"] = json!(index.to_string());
            version
        })
        .collect::<Vec<_>>();
    assert!(
        parse(&json!({"schema_version":2,"plugins":[{"id":"sample","versions":versions}]}))
            .is_err()
    );
    let mut value = single();
    let template = value["artifacts"][0].clone();
    value["artifacts"] = Value::Array(
        (0..=MAX_ARTIFACTS)
            .map(|index| {
                let mut artifact = template.clone();
                artifact["asset"] = json!(format!("package-{index}.tar.gz"));
                artifact
            })
            .collect(),
    );
    assert!(parse(&value).is_err());
}

#[test]
fn package_member_and_expansion_limits_are_checked_on_declared_bytes() {
    let mut value = single();
    for index in 0..MAX_MEMBERS - 2 {
        value["artifacts"][0]["members"].as_array_mut().unwrap().push(json!({
            "path":format!("data/file-{index}"),"size":1,"sha256":"f".repeat(64),"executable":false}));
    }
    assert_eq!(
        parse(&value).unwrap().plugins[0].versions[0].artifacts[0]
            .members
            .len(),
        MAX_MEMBERS
    );
    value["artifacts"][0]["members"]
        .as_array_mut()
        .unwrap()
        .push(json!({
        "path":"data/one-too-many","size":1,"sha256":"f".repeat(64),"executable":false}));
    assert_eq!(
        parse(&value).unwrap_err().0,
        "package member count exceeds its bounds"
    );
    let mut value = single();
    value["artifacts"][0]["members"][1]["size"] = json!(MAX_PACKAGE_BYTES + 1);
    assert!(parse(&value).is_err());
    value["artifacts"][0]["members"][1]["size"] = json!(MAX_PACKAGE_BYTES);
    for index in 0..3 {
        value["artifacts"][0]["members"].as_array_mut().unwrap().push(json!({
            "path":format!("data/big-{index}"),"size":MAX_PACKAGE_BYTES,"sha256":"f".repeat(64),"executable":false}));
    }
    assert_eq!(
        parse(&value).unwrap_err().0,
        "expanded package bytes exceed their bounds"
    );
    value["artifacts"][0]["members"][4]["size"] = json!(MAX_PACKAGE_BYTES - 1024);
    assert!(parse(&value).is_ok());
}

#[test]
fn platform_selection_uses_declarations_without_architecture_or_abi_fallback() {
    let mut value = single();
    let mut arm = value["artifacts"][0].clone();
    arm["architecture"] = json!("arm64");
    arm["asset"] = json!("sample-arm64.tar.gz");
    value["artifacts"].as_array_mut().unwrap().push(arm);
    let catalogue = parse(&value).unwrap();
    let version = &catalogue.plugins[0].versions[0];
    let host = Platform {
        architecture: Architecture::Arm64,
        libc: Some((Libc::Glibc, "2.39".into())),
    };
    assert_eq!(version.select(&host).unwrap().asset, "sample-arm64.tar.gz");
    let mut amd64 = host.clone();
    amd64.architecture = Architecture::Amd64;
    assert_eq!(version.select(&amd64).unwrap().asset, "sample-amd64.tar.gz");
    let mut old = host.clone();
    old.libc = Some((Libc::Glibc, "2.34".into()));
    assert!(version.select(&old).is_err());
    let mut unknown = host.clone();
    unknown.libc = None;
    assert!(version.select(&unknown).is_err());
    let mut musl = host.clone();
    musl.libc = Some((Libc::Musl, "2.39".into()));
    assert!(version.select(&musl).is_err());
    let mut newer_numeric = host.clone();
    newer_numeric.libc = Some((Libc::Glibc, "2.100".into()));
    assert_eq!(
        version.select(&newer_numeric).unwrap().asset,
        "sample-arm64.tar.gz"
    );
    let mut same = host.clone();
    same.libc = Some((Libc::Glibc, "2.35.0".into()));
    assert_eq!(version.select(&same).unwrap().asset, "sample-arm64.tar.gz");
    let mut duplicate = value["artifacts"][1].clone();
    duplicate["asset"] = json!("sample-arm64-alternative.tar.gz");
    value["artifacts"].as_array_mut().unwrap().push(duplicate);
    let catalogue = parse(&value).unwrap();
    assert!(catalogue.plugins[0].versions[0].select(&host).is_err());
    let catalogue = parse(&single()).unwrap();
    assert!(catalogue.plugins[0].versions[0].select(&host).is_err());
}

#[test]
fn script_runtimes_and_dependencies_need_installed_executables_not_guessed_versions() {
    let mut value = single();
    value["artifacts"][0]["runtime"] =
        json!({"kind":"script","executable":"/usr/bin/sh","minimum":null});
    let catalogue = parse(&value).unwrap();
    let mut host = Platform::current().unwrap();
    host.architecture = Architecture::Amd64;
    assert_eq!(
        catalogue.plugins[0].versions[0]
            .select(&host)
            .unwrap()
            .asset,
        "sample-amd64.tar.gz"
    );
    value["artifacts"][0]["runtime"]["minimum"] = json!("1.0");
    let catalogue = parse(&value).unwrap();
    assert_eq!(
        catalogue.plugins[0].versions[0].artifacts[0]
            .compatibility(&host)
            .unwrap_err()
            .0,
        "the declared executable minimum version cannot be established without a probe"
    );
    value["artifacts"][0]["runtime"]["minimum"] = Value::Null;
    value["artifacts"][0]["dependencies"] = json!([{"executable":format!("/usr/bin/among-absent-{}",glib::uuid_string_random()),"minimum":null}]);
    let catalogue = parse(&value).unwrap();
    assert!(catalogue.plugins[0].versions[0].select(&host).is_err());
    for executable in ["sh -c", "/usr/bin/../bin/sh", "/tmp/runtime", "/usr/bin/."] {
        let mut value = single();
        value["artifacts"][0]["runtime"] =
            json!({"kind":"script","executable":executable,"minimum":null});
        assert!(parse(&value).is_err());
    }
    let mut value = single();
    value["artifacts"][0]["runtime"] =
        json!({"kind":"script","executable":"/usr/bin/env","minimum":null});
    let catalogue = parse(&value).unwrap();
    assert_eq!(
        catalogue.plugins[0].versions[0].artifacts[0]
            .compatibility(&host)
            .unwrap_err()
            .0,
        "env-based script runtimes are not modeled",
        "retained cache metadata remains parseable but unsupported wrapper selection is refused"
    );
}

fn carried(id: &str, release: Value) -> Value {
    let mut value = version(id);
    value["release"] = release;
    value
}

fn catalogue_of(versions: Vec<Value>) -> Value {
    let plugins = versions
        .into_iter()
        .map(|version| json!({"id":version["manifest"]["id"],"versions":[version]}))
        .collect::<Vec<_>>();
    json!({"schema_version":2,"plugins":plugins})
}

#[test]
fn a_catalogue_made_by_the_release_tool_carry_over_parses_and_keeps_an_existing_reference() {
    use crate::plugin::install::source::tests::carried_releases::carry_over;
    let first_release = catalogue_of(vec![version("first"), version("second")]);
    assert!(parse(&first_release).is_ok());
    // The second release changes `first`; `second` is carried from the first.
    let mut changed = version("first");
    changed["revision"] = json!("e".repeat(40));
    changed["manifest"]["version"] = json!("1.1.0");
    let mut rebuilt = version("second");
    rebuilt["revision"] = json!("e".repeat(40));
    let second_release = carry_over(
        &first_release,
        &catalogue_of(vec![changed.clone(), rebuilt.clone()]),
        &["second"],
        434653882,
        "v0.2.0",
    );
    assert_eq!(
        second_release["plugins"][1]["versions"][0]["release"],
        json!({"id":434653882u64,"tag":"v0.2.0"})
    );
    assert_eq!(
        second_release["plugins"][1]["versions"][0]["revision"],
        "a".repeat(40)
    );
    let original = parse(&first_release)
        .unwrap()
        .plugins
        .remove(1)
        .versions
        .remove(0);
    let parsed = parse(&second_release).unwrap();
    assert_eq!(parsed.plugins[0].versions[0].release, None);
    let mut kept = parsed.plugins[1].versions[0].clone();
    assert_eq!(
        kept.release.take(),
        Some(ReleaseReference {
            id: 434653882,
            tag: "v0.2.0".into()
        })
    );
    assert_eq!(kept, original, "only the reference was added");
    assert_eq!(parsed.references().unwrap().len(), 1);
    // The third release changes `first` again; the entry of `second` keeps
    // the reference to the release that holds its files.
    changed["revision"] = json!("f".repeat(40));
    changed["manifest"]["version"] = json!("1.2.0");
    rebuilt["revision"] = json!("f".repeat(40));
    let third_release = carry_over(
        &second_release,
        &catalogue_of(vec![changed, rebuilt]),
        &["second"],
        434999999,
        "v0.3.0",
    );
    assert_eq!(
        third_release["plugins"][1], second_release["plugins"][1],
        "carried verbatim"
    );
    assert_eq!(parse(&third_release).unwrap().plugins[1], parsed.plugins[1]);
    // The single-version form reads the same envelope.
    let mut alone = single();
    alone["release"] = json!({"id":434653882u64,"tag":"v0.2.0"});
    assert_eq!(
        parse(&alone).unwrap().plugins[0].versions[0]
            .release
            .as_ref()
            .map(|reference| reference.id),
        Some(434653882)
    );
}

#[test]
fn a_malformed_release_reference_is_refused() {
    for release in [
        Value::Null,
        json!({}),
        json!({"id":5}),
        json!({"tag":"v0.9"}),
        json!({"id":0,"tag":"v0.9"}),
        json!({"id":-5,"tag":"v0.9"}),
        json!({"id":"5","tag":"v0.9"}),
        json!({"id":5.5,"tag":"v0.9"}),
        json!({"id":null,"tag":"v0.9"}),
        json!({"id":5,"tag":""}),
        json!({"id":5,"tag":9}),
        json!({"id":5,"tag":null}),
        json!({"id":5,"tag":"v0..9"}),
        json!({"id":5,"tag":"v0.9 final"}),
        json!({"id":5,"tag":"refs/tags/.v0.9"}),
        json!({"id":5,"tag":"v0.9","repository":"example/other"}),
        json!({"id":5,"tag":"v0.9","repository_id":13}),
        json!({"id":5,"tag":"v0.9","asset_id":51}),
        json!([5, "v0.9"]),
        json!("v0.9"),
        json!(5),
    ] {
        let document = catalogue_of(vec![version("first"), carried("second", release.clone())]);
        assert!(parse(&document).is_err(), "{release}");
    }
    // The same key under another name or at another level stays unknown.
    let mut misplaced = catalogue_of(vec![version("first"), version("second")]);
    misplaced["plugins"][1]["release"] = json!({"id":5,"tag":"v0.9"});
    assert!(parse(&misplaced).is_err());
    let mut misplaced = catalogue_of(vec![version("first"), version("second")]);
    misplaced["release"] = json!({"id":5,"tag":"v0.9"});
    assert!(parse(&misplaced).is_err());
    let mut artifact = catalogue_of(vec![version("first"), version("second")]);
    artifact["plugins"][1]["versions"][0]["artifacts"][0]["release"] = json!({"id":5,"tag":"v0.9"});
    assert!(parse(&artifact).is_err());
}

#[test]
fn distinct_referenced_releases_are_bounded_and_named_consistently() {
    let referring = |count: u64| {
        let mut versions = vec![version("fresh")];
        for index in 0..count {
            versions.push(carried(
                &format!("kept-{index}"),
                json!({"id": 100 + index, "tag": format!("v0.{index}")}),
            ));
        }
        catalogue_of(versions)
    };
    let most = MAX_CARRIED_RELEASES as u64;
    assert_eq!(
        parse(&referring(most))
            .unwrap()
            .references()
            .unwrap()
            .iter()
            .map(|reference| reference.id)
            .collect::<Vec<_>>(),
        (100..100 + most).collect::<Vec<_>>()
    );
    assert_eq!(
        parse(&referring(most + 1)).unwrap_err().0,
        "catalogue refers to more earlier releases than the host follows"
    );
    // Many plugins held by one release are one referenced release.
    let mut versions = vec![version("fresh")];
    for index in 0..20 {
        versions.push(carried(
            &format!("kept-{index}"),
            json!({"id":100,"tag":"v0.1"}),
        ));
    }
    parse(&catalogue_of(versions)).unwrap();
    // One release has one tag and one tag names one release.
    for other in [
        json!({"id":100,"tag":"v0.2"}),
        json!({"id":101,"tag":"v0.1"}),
    ] {
        let document = catalogue_of(vec![
            version("fresh"),
            carried("kept", json!({"id":100,"tag":"v0.1"})),
            carried("other", other.clone()),
        ]);
        assert_eq!(
            parse(&document).unwrap_err().0,
            "catalogue names one earlier release under two identities",
            "{other}"
        );
    }
}
