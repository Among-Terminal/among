use super::*;
use crate::plugin_install::release::{self, fixture::Fixture};
use std::fs;

#[test]
fn current_schema2_observation_reopens_real_files_without_historical_pins() -> Result<()> {
    let fixture = Fixture::new()?;
    assert_eq!(release::verify(&fixture.root)?, fixture.inputs);
    assert_eq!(release::profile_inputs(&fixture.root)?, fixture.inputs);
    assert_eq!(
        release::catalogue(&fixture.root, &fixture.inputs)?,
        fixture.catalogue
    );
    Ok(())
}

#[test]
fn corrupt_release_package_catalogue_and_tag_facts_refuse_without_rewriting_files() -> Result<()> {
    for field in [
        "schema",
        "publisher",
        "release",
        "catalogue",
        "revision",
        "asset",
        "binding",
        "cycle",
        "snapshot",
        "source",
    ] {
        let mut fixture = Fixture::new()?;
        let catalogue_before = fs::read(fixture.root.join("among-plugins.json"))?;
        match field {
            "schema" => fixture.inputs["schema_version"] = json!(1),
            "publisher" => fixture.inputs["publisher_id"] = json!(PUBLISHER + 1),
            "release" => fixture.inputs["release_id"] = json!(901),
            "catalogue" => fixture.inputs["catalogue_sha256"] = json!("b".repeat(64)),
            "revision" => fixture.inputs["source_revision"] = json!("c".repeat(40)),
            "asset" => fixture.inputs["plugins"][0]["asset_id"] = json!(999),
            "binding" => fixture.inputs["release_binding"]["object"] = json!("d".repeat(40)),
            "cycle" => {
                fixture.inputs["tag_chain"] =
                    json!([["tag", "a".repeat(40)], ["commit", "a".repeat(40)]])
            }
            "snapshot" => {
                fixture.inputs["release_snapshot"]["assets"]
                    .as_array_mut()
                    .ok_or("fixture assets absent")?
                    .pop();
            }
            _ => fixture.inputs["source_sha256"] = json!("e".repeat(64)),
        }
        fixture.save()?;
        let before = fs::read(fixture.root.join("inputs.json"))?;
        assert!(release::verify(&fixture.root).is_err(), "{field}");
        assert!(release::profile_inputs(&fixture.root).is_err(), "{field}");
        assert_eq!(fs::read(fixture.root.join("inputs.json"))?, before);
        assert_eq!(
            fs::read(fixture.root.join("among-plugins.json"))?,
            catalogue_before
        );
    }
    Ok(())
}

#[test]
fn catalogue_refuses_schema1_embedded_ids_and_missing_other_architecture_bindings() -> Result<()> {
    let fixture = Fixture::new()?;
    let architecture = text(&fixture.inputs, "architecture")?;
    for mutation in [
        "schema",
        "release_id",
        "asset_id",
        "other_arch_digest",
        "member",
    ] {
        let mut catalogue = fixture.catalogue.clone();
        let version = &mut catalogue["plugins"][0]["versions"][0];
        match mutation {
            "schema" => catalogue["schema_version"] = json!(1),
            "release_id" => version["release_id"] = json!(900),
            "asset_id" => version["artifacts"][0]["asset_id"] = json!(100),
            "other_arch_digest" => version["artifacts"][1]["sha256"] = json!("f".repeat(64)),
            _ => version["artifacts"][0]["members"][0]["sha256"] = json!("f".repeat(64)),
        }
        assert!(
            validate(&fixture.inputs, &catalogue, architecture).is_err(),
            "{mutation}"
        );
    }
    Ok(())
}

#[test]
fn complete_api_inventory_refuses_missing_flags_duplicate_names_and_invalid_asset_digests(
) -> Result<()> {
    let fixture = Fixture::new()?;
    let assets = fixture.inputs["release_snapshot"]["assets"]
        .as_array()
        .ok_or("fixture assets absent")?
        .iter()
        .map(|entry| {
            Ok(
                json!({"id":entry["id"],"name":entry["name"],"size":entry["size"],
                "state":"uploaded","digest":format!("sha256:{}",text(entry, "sha256")?)}),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let release = json!({"id":900,"tag_name":"v0.2.0","draft":false,"prerelease":false,"immutable":true,"assets":assets});
    assert_eq!(snapshot(&release)?, fixture.inputs["release_snapshot"]);
    for mutation in [
        "draft",
        "prerelease",
        "immutable",
        "missing",
        "duplicate",
        "digest",
        "overflow",
    ] {
        let mut value = release.clone();
        match mutation {
            "draft" => value["draft"] = json!(true),
            "prerelease" => value["prerelease"] = json!(true),
            "immutable" => value["immutable"] = json!(false),
            "missing" => {
                value
                    .as_object_mut()
                    .ok_or("fixture release is not an object")?
                    .remove("immutable");
            }
            "duplicate" => value["assets"][1]["name"] = value["assets"][0]["name"].clone(),
            "digest" => value["assets"][1]["digest"] = Value::Null,
            _ => {
                let entry = value["assets"][0].clone();
                value["assets"] = json!(vec![entry; 65]);
            }
        }
        assert!(snapshot(&value).is_err(), "{mutation}");
    }
    Ok(())
}

#[test]
fn selection_returns_each_plugin_for_the_requested_native_architecture_only() -> Result<()> {
    let fixture = Fixture::new()?;
    for architecture in ["amd64", "arm64"] {
        let selected = select(&fixture.catalogue, architecture)?;
        assert_eq!(selected.len(), 3);
        for (version, artifact) in selected {
            assert_eq!(version["revision"], fixture.inputs["source_revision"]);
            assert_eq!(artifact["architecture"], architecture);
            assert_eq!(
                artifact["asset"],
                format!(
                    "{}-0.2.0-linux-{architecture}.tar.gz",
                    text(&version["manifest"], "id")?
                )
            );
        }
    }
    for architecture in ["", "x86_64", "aarch64", "riscv64", "latest"] {
        assert!(
            select(&fixture.catalogue, architecture).is_err(),
            "{architecture}"
        );
    }
    Ok(())
}

#[test]
fn selection_refuses_duplicated_plugins_architectures_and_package_dependencies() -> Result<()> {
    let fixture = Fixture::new()?;
    for mutation in [
        "plugin",
        "architecture",
        "dependencies",
        "unknown",
        "versions",
    ] {
        let mut catalogue = fixture.catalogue.clone();
        match mutation {
            "plugin" => catalogue["plugins"][1] = catalogue["plugins"][0].clone(),
            "architecture" => {
                catalogue["plugins"][0]["versions"][0]["artifacts"][0]["architecture"] =
                    json!("arm64")
            }
            "dependencies" => {
                catalogue["plugins"][0]["versions"][0]["artifacts"][1]["dependencies"] =
                    json!(["compile-locally"])
            }
            "unknown" => catalogue["plugins"][0]["id"] = json!("other"),
            _ => {
                let version = catalogue["plugins"][0]["versions"][0].clone();
                catalogue["plugins"][0]["versions"] = json!([version.clone(), version]);
            }
        }
        assert!(select(&catalogue, "amd64").is_err(), "{mutation}");
    }
    Ok(())
}
