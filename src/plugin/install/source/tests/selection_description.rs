use super::*;
use crate::plugin::install::metadata::Architecture;
use serde_json::json;

#[test]
fn parsed_versions_describe_the_native_selected_asset_not_an_alternative() {
    let (native, other) = match Platform::current().unwrap().architecture {
        Architecture::Amd64 => ("amd64", "arm64"),
        Architecture::Arm64 => ("arm64", "amd64"),
    };
    for (version, asset_id, size) in [("0.1.0-rc.1", 71, 123), ("2.7", 91, 456)] {
        let mut metadata = super::super::super::metadata::tests::single();
        metadata["manifest"]["version"] = json!(version);
        let mut alternative = metadata["artifacts"][0].clone();
        alternative["architecture"] = json!(other);
        alternative["asset"] = json!(format!("other-{}.tar.gz", asset_id + 1));
        metadata["artifacts"][0]["architecture"] = json!(native);
        metadata["artifacts"][0]["runtime"]["minimum"] = json!("2.0");
        let asset = format!("native-{asset_id}.tar.gz");
        metadata["artifacts"][0]["asset"] = json!(asset);
        metadata["artifacts"][0]["size"] = json!(size);
        // Put the incompatible target first so the label cannot follow array order.
        metadata["artifacts"]
            .as_array_mut()
            .unwrap()
            .insert(0, alternative);
        let inspection = Inspection::from_bytes(
            Origin {
                repository_id: 12,
                publisher_id: 34,
                owner: "example".into(),
                repository: "sample".into(),
            },
            "a".repeat(40),
            metadata.to_string().into_bytes(),
        )
        .unwrap();
        let selection = inspection.select("sample", version).unwrap();
        let expected = format!(
            "github:12/sample · version {version} · linux/{native} · asset {asset} · {size} bytes"
        );
        assert_eq!(selection.artifact.asset, asset);
        assert_eq!(selection.description(), expected);
        assert!(selection
            .review_description()
            .starts_with(&format!("{expected}\n")));
        assert!(!selection.description().contains(&format!("linux/{other}")));
        assert!(inspection.select("sample", "latest").is_err());
    }
}

#[test]
fn parsed_third_party_script_selection_keeps_its_version_target_and_runtime() {
    let native = match Platform::current().unwrap().architecture {
        Architecture::Amd64 => "amd64",
        Architecture::Arm64 => "arm64",
    };
    let mut metadata = super::super::super::metadata::tests::single();
    metadata["artifacts"][0]["architecture"] = json!(native);
    metadata["artifacts"][0]["runtime"] =
        json!({"kind":"script","executable":"/usr/bin/sh","minimum":null});
    // Version text remains data; the UI consumer, not Selection, escapes markup.
    metadata["manifest"]["version"] = json!("third-party<&>");
    let inspection = Inspection::from_bytes(
        Origin {
            repository_id: 99,
            publisher_id: 88,
            owner: "third-party".into(),
            repository: "scripts".into(),
        },
        "a".repeat(40),
        metadata.to_string().into_bytes(),
    )
    .unwrap();
    let selection = inspection.select("sample", "third-party<&>").unwrap();
    assert_eq!(
        selection.description(),
        format!(
            "github:99/sample · version third-party<&> · linux/{native} · asset sample-amd64.tar.gz · 123 bytes"
        )
    );
    assert!(
        matches!(selection.artifact.runtime, super::super::super::metadata::Runtime::Script { ref executable, minimum: None } if executable == "/usr/bin/sh")
    );
    assert!(selection
        .review_description()
        .starts_with(&selection.description()));
    metadata["artifacts"][0]["runtime"]["minimum"] = json!("1.0");
    let inspection = Inspection::from_bytes(
        inspection.origin(),
        "a".repeat(40),
        metadata.to_string().into_bytes(),
    )
    .unwrap();
    assert!(inspection.select("sample", "third-party<&>").is_err());
}
