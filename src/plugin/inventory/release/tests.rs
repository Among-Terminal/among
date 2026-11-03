use super::*;
use serde_json::json;

fn asset(id: u64, name: &str) -> Value {
    json!({"id":id,"name":name,"state":"uploaded","size":123,"digest":format!("sha256:{}","a".repeat(64))})
}

fn release() -> Value {
    json!({"id":7,"draft":false,"prerelease":false,"immutable":true,"tag_name":"v1.0",
        "assets":[asset(11,"sample-amd64.tar.gz"),asset(12,"among-plugins.json"),asset(13,"NOTICES")]})
}

#[test]
fn complete_observed_release_is_canonical_and_binds_every_asset() {
    let value = release();
    let selected = ReleaseSnapshot::from_value(&value).unwrap();
    assert_eq!(selected.id, 7);
    assert_eq!(selected.tag, "v1.0");
    assert_eq!(selected.assets.len(), 3);
    let package = selected.asset("sample-amd64.tar.gz").unwrap();
    assert_eq!(package.name, "sample-amd64.tar.gz");
    assert_eq!(package.size, 123);
    assert_eq!(package.sha256, "a".repeat(64));
    assert!(selected.asset("sample-amd64").is_err());
    let mut reordered = value.clone();
    reordered["assets"].as_array_mut().unwrap().reverse();
    assert_eq!(ReleaseSnapshot::from_value(&reordered).unwrap(), selected);
    for (key, changed) in [
        ("id", json!(14)),
        ("name", json!("different-NOTICES")),
        ("size", json!(124)),
        ("digest", json!(format!("sha256:{}", "b".repeat(64)))),
    ] {
        let mut changed_release = value.clone();
        changed_release["assets"][2][key] = changed;
        assert_ne!(
            ReleaseSnapshot::from_value(&changed_release).unwrap(),
            selected,
            "unselected asset {key} must remain part of the snapshot"
        );
    }
}

#[test]
fn publication_flags_and_exact_release_identity_are_mandatory() {
    for (key, bad) in [
        ("draft", json!(true)),
        ("draft", Value::Null),
        ("prerelease", json!(true)),
        ("prerelease", Value::Null),
        ("immutable", json!(false)),
        ("immutable", Value::Null),
        ("id", json!(0)),
        ("id", json!(-1)),
        ("id", json!("7")),
        ("tag_name", json!("../heads/main")),
        ("tag_name", Value::Null),
    ] {
        let mut value = release();
        value[key] = bad;
        assert!(ReleaseSnapshot::from_value(&value).is_err(), "{key}");
        let mut value = release();
        value.as_object_mut().unwrap().remove(key);
        assert!(
            ReleaseSnapshot::from_value(&value).is_err(),
            "missing {key}"
        );
    }
}

#[test]
fn every_asset_requires_an_uploaded_unique_name_identity_size_and_digest() {
    for index in 0..3 {
        for (key, bad) in [
            ("id", json!(0)),
            ("id", json!("11")),
            ("name", json!("../sample.tar.gz")),
            ("name", json!("dir\\sample.tar.gz")),
            ("name", json!(".")),
            ("name", json!("a\nb")),
            ("name", json!("a".repeat(256))),
            ("state", json!("starter")),
            ("size", json!(-1)),
            ("size", json!("123")),
            ("digest", Value::Null),
            ("digest", json!("sha256:abc")),
            ("digest", json!(format!("SHA256:{}", "a".repeat(64)))),
            ("digest", json!(format!("sha256:{}", "A".repeat(64)))),
            ("digest", json!(format!("sha256:{}", "a".repeat(65)))),
        ] {
            let mut value = release();
            value["assets"][index][key] = bad;
            assert!(
                ReleaseSnapshot::from_value(&value).is_err(),
                "asset {index} {key}"
            );
            let mut value = release();
            value["assets"][index].as_object_mut().unwrap().remove(key);
            assert!(
                ReleaseSnapshot::from_value(&value).is_err(),
                "asset {index} missing {key}"
            );
        }
    }
    for key in ["id", "name"] {
        let mut value = release();
        value["assets"][2][key] = value["assets"][0][key].clone();
        assert!(
            ReleaseSnapshot::from_value(&value).is_err(),
            "duplicate {key}"
        );
    }
}

#[test]
fn inventory_requires_one_catalogue_and_enforces_the_complete_asset_bound() {
    for assets in [Value::Null, json!([]), json!({})] {
        let mut value = release();
        value["assets"] = assets;
        assert!(ReleaseSnapshot::from_value(&value).is_err());
    }
    let mut value = release();
    value["assets"][1]["name"] = json!("other.json");
    assert!(ReleaseSnapshot::from_value(&value).is_err());
    let mut value = release();
    value["assets"][2]["name"] = json!("among-plugins.json");
    assert!(ReleaseSnapshot::from_value(&value).is_err());
    let mut value = release();
    let mut assets = vec![asset(1, "among-plugins.json")];
    for index in 1..MAX_RELEASE_ASSETS {
        assets.push(asset(index as u64 + 1, &format!("file-{index}")));
    }
    value["assets"] = json!(assets);
    assert_eq!(
        ReleaseSnapshot::from_value(&value).unwrap().assets.len(),
        MAX_RELEASE_ASSETS
    );
    value["assets"]
        .as_array_mut()
        .unwrap()
        .push(asset(65, "file-64"));
    assert!(ReleaseSnapshot::from_value(&value).is_err());
}

fn prerelease() -> Value {
    let mut value = release();
    value["prerelease"] = json!(true);
    value
}

#[test]
fn a_prerelease_is_admitted_only_by_its_own_constructor() {
    let snapshot = ReleaseSnapshot::from_prerelease_value(&prerelease()).unwrap();
    assert!(snapshot.is_prerelease());
    assert!(ReleaseSnapshot::from_value(&prerelease()).is_err());
    let stable = ReleaseSnapshot::from_value(&release()).unwrap();
    assert!(!stable.is_prerelease());
    assert!(ReleaseSnapshot::from_prerelease_value(&release()).is_err());
    // Same id, tag and assets still differ by channel, so a pre-commit
    // comparison cannot take one for the other.
    assert_ne!(snapshot, stable);
    for (key, bad) in [
        ("draft", json!(true)),
        ("draft", Value::Null),
        ("prerelease", Value::Null),
        ("immutable", json!(false)),
        ("immutable", Value::Null),
    ] {
        let mut value = prerelease();
        value[key] = bad;
        assert_eq!(
            ReleaseSnapshot::from_prerelease_value(&value).unwrap_err(),
            Problem("development installation requires a published immutable prerelease"),
            "{key}"
        );
    }
}

#[test]
fn a_prerelease_inventory_defect_is_refused_with_the_ordinary_reason() {
    type Defect = fn(&mut Value);
    let defects: [(&str, Defect); 7] = [
        ("tag", |value| value["tag_name"] = json!("../heads/main")),
        ("no assets", |value| value["assets"] = json!([])),
        ("not uploaded", |value| {
            value["assets"][2]["state"] = json!("starter")
        }),
        ("uppercase digest", |value| {
            value["assets"][0]["digest"] = json!(format!("sha256:{}", "A".repeat(64)))
        }),
        ("duplicate name", |value| {
            value["assets"][2]["name"] = json!("sample-amd64.tar.gz")
        }),
        ("no catalogue", |value| {
            value["assets"][1]["name"] = json!("other.json")
        }),
        ("too many assets", |value| {
            for id in 14..80 {
                value["assets"]
                    .as_array_mut()
                    .unwrap()
                    .push(asset(id, &format!("extra-{id}")));
            }
        }),
    ];
    for (name, defect) in defects {
        let mut stable = release();
        defect(&mut stable);
        let mut development = prerelease();
        defect(&mut development);
        let ordinary = ReleaseSnapshot::from_value(&stable).unwrap_err();
        assert_eq!(
            ReleaseSnapshot::from_prerelease_value(&development).unwrap_err(),
            ordinary,
            "{name}"
        );
    }
}

#[test]
fn a_stored_prerelease_is_readmitted_as_a_prerelease_and_stable_json_is_unchanged() {
    let stable = ReleaseSnapshot::from_value(&release()).unwrap();
    let stored = serde_json::to_value(&stable).unwrap();
    assert!(
        stored.get("prerelease").is_none(),
        "ordinary snapshots keep their earlier stored form"
    );
    let reopened: ReleaseSnapshot = serde_json::from_value(stored).unwrap();
    reopened.validate().unwrap();
    assert!(!reopened.is_prerelease());

    let snapshot = ReleaseSnapshot::from_prerelease_value(&prerelease()).unwrap();
    let stored = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(stored["prerelease"], json!(true));
    let reopened: ReleaseSnapshot = serde_json::from_value(stored.clone()).unwrap();
    reopened.validate().unwrap();
    assert!(reopened.is_prerelease());
    assert_eq!(reopened, snapshot);
    // A stored prerelease is still bound by every inventory rule on reopening.
    let mut broken = stored;
    broken["assets"][0]["sha256"] = json!("A".repeat(64));
    let broken: ReleaseSnapshot = serde_json::from_value(broken).unwrap();
    assert!(broken.validate().is_err());
}
