use among_tools::{deb_previous, release_assets, Result};
use serde_json::{json, Value};
use std::path::Path;

fn pin() -> Value {
    let mut packages = serde_json::Map::new();
    for target in release_assets::DEB_TARGETS {
        for architecture in release_assets::ARCHITECTURES {
            packages.insert(
                format!("{target}-{architecture}"),
                json!({
                    "asset": release_assets::package_asset(target, architecture, "deb"),
                    "sha256": "a".repeat(64),
                    "version": format!("0.1.1-1~{target}"),
                }),
            );
        }
    }
    json!({"schema": 1, "tag": "v0.1.1", "packages": packages})
}

fn parse(value: &Value) -> Result<usize> {
    Ok(deb_previous::parse(&serde_json::to_vec(value)?)?.len())
}

/// Release CI upgrades from this file's release on every Debian slot; a pin
/// that misses a slot, or whose names and versions do not follow from its tag,
/// would make the upgrade check download the wrong package or none at all.
#[test]
fn committed_pin_covers_every_debian_slot_of_a_stable_release() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for target in release_assets::DEB_TARGETS {
        for architecture in release_assets::ARCHITECTURES {
            let package = deb_previous::read(&root, target, architecture)?;
            assert_eq!(
                package.url(),
                format!(
                    "https://github.com/Among-Terminal/among/releases/download/{}/{}",
                    package.tag, package.asset
                )
            );
        }
    }
    assert_eq!(parse(&pin())?, 4);
    Ok(())
}

/// Each damage is a pin that would let the upgrade check use a package other
/// than the published one, or skip a slot silently. The refusal must name the
/// reason, so a passing test cannot come from an unrelated parse error.
#[test]
fn pins_that_do_not_describe_the_published_release_are_refused() -> Result<()> {
    type Damage = fn(&mut Value);
    let cases: [(&str, Damage, &str); 9] = [
        (
            "release candidate tag",
            |pin| pin["tag"] = "v0.1.1-rc.1".into(),
            "expected stable vMAJOR.MINOR.PATCH tag",
        ),
        (
            "missing slot",
            |pin| {
                pin["packages"]
                    .as_object_mut()
                    .map(|packages| packages.remove("ubuntu24.04-arm64"));
            },
            "previous release pin lacks ubuntu24.04-arm64",
        ),
        (
            "unknown slot",
            |pin| pin["packages"]["fedora43-amd64"] = pin["packages"]["debian13-amd64"].clone(),
            "names an unknown Debian slot",
        ),
        (
            "asset of another architecture",
            |pin| {
                pin["packages"]["debian13-amd64"]["asset"] =
                    "among-terminal-debian13-arm64.deb".into()
            },
            "is not the release name of this slot",
        ),
        (
            "version of another target",
            |pin| pin["packages"]["debian13-arm64"]["version"] = "0.1.1-1~ubuntu24.04".into(),
            "does not follow from v0.1.1",
        ),
        (
            "version of another release",
            |pin| pin["packages"]["ubuntu24.04-amd64"]["version"] = "0.1.0-1~ubuntu24.04".into(),
            "does not follow from v0.1.1",
        ),
        (
            "uppercase digest",
            |pin| pin["packages"]["debian13-amd64"]["sha256"] = "A".repeat(64).into(),
            "sha256 is not 64 lowercase hex digits",
        ),
        (
            "extra field",
            |pin| pin["packages"]["debian13-amd64"]["url"] = "https://example.com/x.deb".into(),
            "expected exactly asset, sha256 and version",
        ),
        (
            "extra top-level field",
            |pin| pin["mirror"] = "https://example.com".into(),
            "exactly schema, tag and packages",
        ),
    ];
    for (name, damage, reason) in cases {
        let mut value = pin();
        damage(&mut value);
        let error = parse(&value)
            .err()
            .ok_or(format!("{name} was accepted"))?
            .to_string();
        assert!(error.contains(reason), "{name}: {error}");
    }
    Ok(())
}
