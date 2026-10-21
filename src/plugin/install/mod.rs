//! repository metadata is inert input, not permission to install or execute it.

pub mod metadata;
mod package;
pub mod source;
mod store;

#[cfg(test)]
pub(crate) use package::tests::fixture_for as test_package;
