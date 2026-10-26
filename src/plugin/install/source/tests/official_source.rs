use super::*;
use crate::plugin::install::metadata::{Architecture, Platform};
use serde_json::json;

fn origin(publisher: u64) -> Origin {
    Origin {
        repository_id: official::REPOSITORY_ID,
        publisher_id: publisher,
        owner: "Among-Terminal".into(),
        repository: "among-plugins".into(),
    }
}

#[test]
fn official_selection_pins_numeric_publisher_without_confusing_third_party_labels() {
    let mut metadata = super::super::super::metadata::tests::single();
    metadata["artifacts"][0]["architecture"] =
        json!(match Platform::current().unwrap().architecture {
            Architecture::Amd64 => "amd64",
            Architecture::Arm64 => "arm64",
        });
    metadata["artifacts"][0]["runtime"]["minimum"] = json!("2.0");
    let bytes = metadata.to_string().into_bytes();
    assert!(Inspection::from_bytes(
        origin(official::PUBLISHER_ID + 1),
        "a".repeat(40),
        bytes.clone()
    )
    .is_err());
    let mut inspected = Inspection::from_bytes(
        origin(official::PUBLISHER_ID),
        "a".repeat(40),
        bytes.clone(),
    )
    .unwrap();
    assert!(inspected.select("sample", "1.0").is_ok());
    inspected.publisher_id += 1;
    assert!(inspected.select("sample", "1.0").is_err());
    let mut third_party = origin(official::PUBLISHER_ID + 1);
    third_party.repository_id = 99;
    // Equal display labels are not official authority and remain valid custom origins.
    assert!(Inspection::from_bytes(third_party, "a".repeat(40), bytes)
        .unwrap()
        .select("sample", "1.0")
        .is_ok());
}

#[test]
fn official_transfer_before_download_or_precommit_refuses_real_https_installation() {
    for transferred_on in [1, 2] {
        let (metadata, archive) = crate::plugin::install::test_package("sample");
        let selected = Inspection::from_bytes(
            origin(official::PUBLISHER_ID),
            "a".repeat(40),
            metadata.to_string().into_bytes(),
        )
        .unwrap()
        .select("sample", "1.0")
        .unwrap();
        let root = crate::plugin::install::package::tests::OwnedRoot::new();
        let queries = Arc::new(AtomicUsize::new(0));
        let observed = queries.clone();
        let server = Server::new(move |request| {
            if request.starts_with(&format!("GET /repositories/{} ", official::REPOSITORY_ID)) {
                let query = observed.fetch_add(1, Ordering::SeqCst) + 1;
                return Reply::json(
                    json!({"id":official::REPOSITORY_ID,"owner":{"id":if query >= transferred_on { official::PUBLISHER_ID + 1 } else {official::PUBLISHER_ID}},"private":false}),
                );
            }
            // Same owned HTTPS pipeline/bytes; only test routing maps the public
            // numeric identity to the existing synthetic Release fixture.
            let request = request.replace(&official::REPOSITORY_ID.to_string(), "12");
            let mut reply = package_route(metadata.clone(), archive.clone(), &request);
            for (_, value) in &mut reply.headers {
                *value = value.replace(
                    "release-asset/12/",
                    &format!("release-asset/{}/", official::REPOSITORY_ID),
                );
            }
            reply
        });
        let error = install_at(&server, selected, &root.0).unwrap_err();
        assert!(error.0.contains("publisher"));
        assert_eq!(queries.load(Ordering::SeqCst), transferred_on);
        assert!(!root.0.join("installed.json").exists());
        assert_eq!(
            server
                .requests()
                .iter()
                .any(|request| request.contains("/releases/assets/11 ")),
            transferred_on == 2
        );
    }
}
