//! repository metadata is inert input, not permission to install or execute it.

mod entrypoint;
pub mod local;
pub mod metadata;
pub mod mutation;
mod official;
mod operations;
mod package;
pub mod registry;
pub mod source;
pub(crate) mod store;
mod version;

#[cfg(test)]
pub(crate) use package::tests::fixture_for as test_package;
#[cfg(test)]
pub(crate) use package::tests::managed_fixture as test_managed_package;
#[cfg(test)]
pub(crate) use package::tests::notification_fixture as test_notification_package;
#[cfg(test)]
pub(crate) use package::tests::OwnedRoot as TestOwnedRoot;
