use super::*;

// Only a SemVer-higher version of the installed plugin is an update. Every
// other pairing must be refused before anything is downloaded, and a refusal
// leaves the installed generation untouched.
#[test]
fn only_a_semver_higher_version_is_offered_as_an_update() {
    let root = super::super::super::package::tests::OwnedRoot::new();
    let (metadata, archive) = super::super::super::package::tests::fixture();
    let server =
        Server::new(move |request| package_route(metadata.clone(), archive.clone(), request));
    let selection = || {
        server
            .inspect(true)
            .unwrap()
            .select("sample", "1.0.0")
            .unwrap()
    };
    let installed = install_at(&server, selection(), &root.0).unwrap().receipt;
    let entry = |installed_version: &str| {
        let mut entry = crate::plugin::inventory::read(&root.0).unwrap().remove(0);
        entry.inspected.as_mut().unwrap().manifest.version = installed_version.into();
        entry
    };
    let offered = |offered_version: &str| {
        let mut offered = selection();
        offered.version.manifest.version = offered_version.into();
        offered
    };

    // Higher: offered, including a release that follows its own pre-release.
    for (current, higher) in [
        ("1.0.0", "1.0.1"),
        ("1.9.0", "1.10.0"),
        ("1.0.0-rc.1", "1.0.0"),
        ("1.0.0-rc.2", "1.0.0-rc.10"),
        ("0.9.9", "1.0.0"),
    ] {
        offered(higher)
            .for_update(&entry(current), None)
            .unwrap_or_else(|problem| panic!("{current} -> {higher}: {problem}"));
    }

    // Lower: refused with the downgrade reason, never offered.
    for (current, lower) in [
        ("1.0.1", "1.0.0"),
        ("1.10.0", "1.9.0"),
        ("1.0.0", "1.0.0-rc.1"),
        ("1.0.0-rc.10", "1.0.0-rc.2"),
        ("1.0.0", "0.9.9"),
    ] {
        let refused = offered(lower)
            .for_update(&entry(current), None)
            .expect_err(&format!("{current} -> {lower} must not be an update"));
        assert!(refused.0.contains("lower than the installed"), "{refused}");
    }

    // Outside SemVer: no order exists, so neither side may be called newer.
    for (current, other) in [
        ("1.0", "1.0.1"),
        ("1.0.0", "1.1"),
        ("2026.10", "2026.11"),
        ("v1.0.0", "v1.0.1"),
        ("1.0.0", "latest"),
        ("1.0.0", "01.0.1"),
    ] {
        let refused = offered(other)
            .for_update(&entry(current), None)
            .expect_err(&format!("{current} -> {other} has no defined order"));
        assert!(refused.0.contains("Semantic Versioning"), "{refused}");
    }

    // The same version keeps its byte check: equal precedence with other
    // content is a publisher inconsistency, not an update.
    let mut reused = offered("1.0.0+other-build");
    reused.version.manifest_sha256 = "b".repeat(64);
    let refused = reused.for_update(&entry("1.0.0"), None).unwrap_err();
    assert!(refused.0.contains("publisher inconsistency"), "{refused}");

    // The same bytes under the same version are not an update either: an
    // ordinary install is simply already there, with or without build
    // metadata that SemVer ignores.
    for same in ["1.0.0", "1.0.0+001", "1.0.0+other-build"] {
        let refused = offered(same).for_update(&entry("1.0.0"), None).unwrap_err();
        assert_eq!(refused.0, "this version is already installed", "{same}");
    }

    // Byte-equal texts outside SemVer name the same version without an
    // order: only the content decides, exactly as for SemVer.
    for text in ["1.0", "2026.10", "latest"] {
        let refused = offered(text).for_update(&entry(text), None).unwrap_err();
        assert_eq!(refused.0, "this version is already installed", "{text}");
        let mut changed = offered(text);
        changed.artifact.sha256 = "b".repeat(64);
        let refused = changed.for_update(&entry(text), None).unwrap_err();
        assert!(
            refused.0.contains("publisher inconsistency"),
            "{text}: {refused}"
        );
        let mut moved = offered(text);
        moved.version.revision = "b".repeat(40);
        let refused = moved.for_update(&entry(text), None).unwrap_err();
        assert!(
            refused.0.contains("publisher inconsistency"),
            "{text}: {refused}"
        );
    }
    // Different texts stay unordered even when they look alike.
    let refused = offered("1.0").for_update(&entry("1.00"), None).unwrap_err();
    assert!(refused.0.contains("Semantic Versioning"), "{refused}");

    // The install review says before consent that such a plugin gets no updates.
    let note = "never offer updates";
    assert!(offered("1.0").review_description().contains(note));
    assert!(!offered("1.0.0").review_description().contains(note));

    assert_eq!(
        crate::plugin::inventory::read(&root.0).unwrap()[0].receipt,
        installed
    );
}
