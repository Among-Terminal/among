//! only numeric identities grant official status; mutable names remain labels.

use crate::plugin::{data::Problem, origin::Origin};

pub(super) const REPOSITORY_ID: u64 = 1_431_055_733;
pub(super) const PUBLISHER_ID: u64 = 339_567_100;
pub(super) const TITLE: &str = "Official plugins for Among Terminal";

pub(super) fn origin() -> Origin {
    Origin {
        repository_id: REPOSITORY_ID,
        publisher_id: PUBLISHER_ID,
        owner: "Among-Terminal".into(),
        repository: "among-plugins".into(),
    }
}

pub(super) fn validate(origin: &Origin) -> Result<(), Problem> {
    origin.validate()?;
    if origin.repository_id == REPOSITORY_ID && origin.publisher_id != PUBLISHER_ID {
        return Err(Problem(
            "official source publisher differs from this application build's pinned identity; a reviewed host change is required",
        ));
    }
    Ok(())
}
