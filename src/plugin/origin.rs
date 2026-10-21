//! labels can change without changing repository authority. publisher authority
//! follows GitHub's numeric owner identity, not a mutable login spelling.

use super::data::Problem;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    pub repository_id: u64,
    pub publisher_id: u64,
    pub owner: String,
    pub repository: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Unchanged,
    Renamed,
    PublisherChanged,
    IdentityChanged,
}

impl Change {
    pub fn description(self) -> &'static str {
        match self {
            Self::Unchanged => "Same repository and publisher identity",
            Self::Renamed => "Repository renamed; numeric repository and publisher identities are unchanged",
            Self::PublisherChanged => "Publisher changed or unconfirmed; explicit renewed source/access review is required",
            Self::IdentityChanged => "Different repository/plugin identity; this is separate code, not an update",
        }
    }
}

impl Origin {
    pub fn validate(&self) -> Result<(), Problem> {
        if self.repository_id == 0
            || self.publisher_id == 0
            || !component(&self.owner)
            || !component(&self.repository)
        {
            return Err(Problem(
                "invalid authoritative repository or publisher identity",
            ));
        }
        Ok(())
    }

    pub fn compare(&self, previous: &Self) -> Change {
        if self.repository_id != previous.repository_id {
            Change::IdentityChanged
        } else if self.publisher_id != previous.publisher_id {
            Change::PublisherChanged
        } else if !self.owner.eq_ignore_ascii_case(&previous.owner)
            || !self.repository.eq_ignore_ascii_case(&previous.repository)
        {
            Change::Renamed
        } else {
            Change::Unchanged
        }
    }
}

pub(super) fn component(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 100
        && !matches!(text, "." | "..")
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::inventory::{self, SourceReceipt};
    use serde_json::json;

    #[test]
    fn numeric_origin_review_survives_rename_but_not_transfer_recreation_or_expanded_access() {
        let root =
            std::env::temp_dir().join(format!("among-origin-{}", glib::uuid_string_random()));
        let mut receipt = inventory::tests::fixture(&root);
        receipt.source = Some(SourceReceipt {
            publisher_id: 34,
            catalogue_revision: "a".repeat(40),
            catalogue_sha256: "b".repeat(64),
        });
        let origin = Origin {
            repository_id: 12,
            publisher_id: 34,
            owner: "example".into(),
            repository: "sample".into(),
        };
        assert_eq!(receipt.origin_change(&origin, "sample"), Change::Unchanged);
        let review = receipt.review(vec!["status".into()]);
        receipt.repository = "renamed".into();
        assert_eq!(receipt.origin_change(&origin, "sample"), Change::Renamed);
        assert!(receipt.matches_review(&review));
        let mut entry = inventory::read(&root).unwrap().remove(0);
        let manifest = &entry.inspected.as_ref().unwrap().manifest;
        assert!(crate::plugin::managed::granted_features(
            manifest,
            &receipt,
            Some(&json!({"review":review}))
        )
        .is_ok());
        receipt.source.as_mut().unwrap().publisher_id = 99;
        assert!(
            !receipt.matches_review(&review),
            "same login cannot inherit a different numeric owner's grant"
        );
        assert!(crate::plugin::managed::granted_features(
            manifest,
            &receipt,
            Some(&json!({"review":review}))
        )
        .is_err());
        let mut transferred = entry.clone();
        transferred.receipt = receipt.clone();
        let mut host = crate::plugin::host::PluginHost::new();
        host.register_managed(transferred, Some(&json!({"enabled":true,"review":review})));
        assert_eq!(host.supervised_count(), 0);
        assert!(
            matches!(&host.listing()[0].1,crate::plugin::host::PluginState::Unavailable{reason} if reason.contains("publisher"))
        );
        assert!(
            !root.join("executed").exists(),
            "retained approval of a different numeric publisher must not execute the real fixture"
        );
        host.disable("github:12/sample");
        host.review_access("github:12/sample", receipt.review(vec!["status".into()]))
            .unwrap();
        host.enable("github:12/sample");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !root.join("executed").exists() {
            host.poll();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        host.shutdown();
        while !host.is_stopped() {
            host.poll();
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(
            receipt.origin_change(&origin, "sample"),
            Change::PublisherChanged
        );
        receipt.repository_id = 55;
        assert_eq!(
            receipt.origin_change(&origin, "sample"),
            Change::IdentityChanged
        );
        assert!(!receipt.matches_review(&review));
        assert!(
            !receipt.matches_review(
                &json!({"owner":"example","repository":"renamed","features":["status"]})
            ),
            "unconfirmed numeric publisher cannot reuse old label-only review"
        );
        entry
            .inspected
            .as_mut()
            .unwrap()
            .manifest
            .required_features
            .push("settings".into());
        receipt.repository_id = 12;
        receipt.source.as_mut().unwrap().publisher_id = 34;
        assert!(crate::plugin::managed::granted_features(
            &entry.inspected.as_ref().unwrap().manifest,
            &receipt,
            Some(&json!({"review":review}))
        )
        .is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
