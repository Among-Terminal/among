//! the plugin list is assembled from saved source snapshots and installed
//! plugins only. nothing in this module can reach the network: it receives
//! decoded snapshots and returns a description, so opening a page never
//! depends on connectivity and an offline start shows what was saved.

use super::{
    registry::{self, Registration},
    source::{Inspection, Selection, UpdateVerdict},
};
use crate::plugin::{data::Problem, inventory::Entry, manifest::Icon, origin::Origin};
use serde_json::Value;

/// What one registered source contributes to the list.
#[derive(Clone, Debug)]
pub struct SourceCatalogue {
    pub origin: Origin,
    pub official: bool,
    pub state: CatalogueState,
}

#[derive(Clone, Debug)]
pub enum CatalogueState {
    /// The source was never checked, or only a prerelease was seen.
    NoSnapshot,
    /// The saved snapshot or the source itself failed validation.
    Unreadable(Problem),
    Loaded {
        inspection: Box<Inspection>,
        retrieved_at: u64,
    },
}

impl SourceCatalogue {
    /// Decodes the saved snapshot of a registration. Reads the stored bytes
    /// only.
    pub fn from_registration(registration: &Registration) -> Self {
        let state = match (registration.available(), registration.cached_inspection()) {
            (Err(problem), _) | (_, Some(Err(problem))) => CatalogueState::Unreadable(problem),
            (Ok(()), None) => CatalogueState::NoSnapshot,
            (Ok(()), Some(Ok(inspection))) => CatalogueState::Loaded {
                retrieved_at: inspection.retrieved_at,
                inspection: Box::new(inspection),
            },
        };
        Self {
            origin: registration.display_origin(),
            official: registration.is_official(),
            state,
        }
    }

    /// A catalogue observed in this session, such as a development
    /// prerelease that the source cache refuses to keep.
    pub fn from_inspection(inspection: Inspection, official: bool) -> Self {
        Self {
            origin: inspection.origin(),
            official,
            state: CatalogueState::Loaded {
                retrieved_at: inspection.retrieved_at,
                inspection: Box::new(inspection),
            },
        }
    }
}

/// An installed plugin and the saved settings its updates are checked against.
#[derive(Clone, Copy)]
pub struct Installed<'a> {
    pub entry: &'a Entry,
    pub stored: Option<&'a Value>,
}

#[derive(Clone, Debug)]
pub struct SourceStatus {
    pub origin: Origin,
    pub official: bool,
    pub state: SourceState,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceState {
    NoSnapshot,
    Unreadable(Problem),
    Saved {
        retrieved_at: u64,
        /// A snapshot a day old or more, or one whose age cannot be told.
        stale: bool,
    },
}

/// One plugin a saved source offers and which is not installed.
#[derive(Clone, Debug)]
pub struct Offer {
    pub source: Origin,
    pub official: bool,
    pub plugin_id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub icon: Option<Icon>,
    /// The download size of the package for this platform, when one exists.
    pub size: Option<u64>,
    /// The selection to install, or why this platform cannot install it.
    pub selection: Result<Selection, Problem>,
}

impl Offer {
    /// Same identity as `Selection::identity`: equal display names from two
    /// sources stay two rows because the repository id differs.
    pub fn key(&self) -> String {
        format!("github:{}/{}", self.source.repository_id, self.plugin_id)
    }
}

/// Where an installed plugin came from, as far as the registered sources know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    /// A registered source published it.
    Source(Origin),
    /// It was published by a repository that is no longer registered.
    SourceRemoved {
        repository_id: u64,
        owner: String,
        repository: String,
    },
    /// A local development install without repository or release.
    Local,
}

#[derive(Clone, Debug)]
pub enum Standing {
    Update(Box<Selection>),
    /// The saved snapshot offers this version or nothing newer.
    Current,
    /// The saved snapshot of its source does not list this plugin.
    NotOffered,
    /// Nothing is known: the source has no usable snapshot, was removed, or
    /// the plugin is a local install.
    Unknown,
    /// A newer-looking version is withheld; the reason is worth showing.
    Withheld(Problem),
}

#[derive(Clone, Debug)]
pub struct InstalledPlugin {
    pub key: String,
    pub link: Link,
    pub standing: Standing,
}

#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub sources: Vec<SourceStatus>,
    pub installed: Vec<InstalledPlugin>,
    pub available: Vec<Offer>,
}

impl Listing {
    /// Whether rows must name their source: only while more than one is
    /// registered.
    pub fn several_sources(&self) -> bool {
        self.sources.len() > 1
    }
}

/// Builds the list from decoded snapshots, official source first. `now` is
/// the current Unix time, passed in so the result depends on nothing else.
pub fn assemble(sources: &[SourceCatalogue], installed: &[Installed<'_>], now: u64) -> Listing {
    let mut listing = Listing::default();
    for source in sources {
        listing.sources.push(SourceStatus {
            origin: source.origin.clone(),
            official: source.official,
            state: match &source.state {
                CatalogueState::NoSnapshot => SourceState::NoSnapshot,
                CatalogueState::Unreadable(problem) => SourceState::Unreadable(*problem),
                CatalogueState::Loaded { retrieved_at, .. } => SourceState::Saved {
                    retrieved_at: *retrieved_at,
                    stale: registry::snapshot_is_stale(*retrieved_at, now),
                },
            },
        });
    }
    for plugin in installed {
        listing.installed.push(describe_installed(sources, *plugin));
    }
    for source in sources {
        let CatalogueState::Loaded { inspection, .. } = &source.state else {
            continue;
        };
        for plugin in &inspection.catalogue.plugins {
            let taken =
                installed.iter().any(|installed| {
                    installed.entry.receipt.plugin_id == plugin.id
                        && installed.entry.receipt.release().is_some_and(|release| {
                            release.repository_id == source.origin.repository_id
                        })
                });
            if taken {
                continue;
            }
            if let Some(offer) = best_offer(inspection, &plugin.id) {
                listing.available.push(Offer {
                    source: source.origin.clone(),
                    official: source.official,
                    ..offer
                });
            }
        }
    }
    listing.available.sort_by(|first, second| {
        second
            .official
            .cmp(&first.official)
            .then_with(|| first.name.cmp(&second.name))
            .then_with(|| first.key().cmp(&second.key()))
    });
    listing
}

fn describe_installed(sources: &[SourceCatalogue], plugin: Installed<'_>) -> InstalledPlugin {
    let key = plugin.entry.key();
    let Some(release) = plugin.entry.receipt.release() else {
        return InstalledPlugin {
            key,
            link: Link::Local,
            standing: Standing::Unknown,
        };
    };
    let Some(source) = sources
        .iter()
        .find(|source| source.origin.repository_id == release.repository_id)
    else {
        return InstalledPlugin {
            key,
            link: Link::SourceRemoved {
                repository_id: release.repository_id,
                owner: release.owner.clone(),
                repository: release.repository.clone(),
            },
            standing: Standing::Unknown,
        };
    };
    let standing = match &source.state {
        CatalogueState::Loaded { inspection, .. } => {
            match best_offer(inspection, &plugin.entry.receipt.plugin_id) {
                None => Standing::NotOffered,
                Some(offer) => match offer.selection {
                    // an offer this platform cannot install is no update.
                    Err(problem) => Standing::Withheld(problem),
                    Ok(selection) => match selection.update_verdict(plugin.entry, plugin.stored) {
                        UpdateVerdict::Update(selection) => Standing::Update(selection),
                        UpdateVerdict::Current => Standing::Current,
                        UpdateVerdict::Withheld(problem) => Standing::Withheld(problem),
                    },
                },
            }
        }
        CatalogueState::NoSnapshot | CatalogueState::Unreadable(_) => Standing::Unknown,
    };
    InstalledPlugin {
        key,
        link: Link::Source(source.origin.clone()),
        standing,
    }
}

/// The one version a release catalogue lists for a plugin (a release
/// catalogue carries exactly one), with its installable selection or the
/// reason this platform cannot install it.
fn best_offer(inspection: &Inspection, plugin_id: &str) -> Option<Offer> {
    let plugin = inspection
        .catalogue
        .plugins
        .iter()
        .find(|plugin| plugin.id == plugin_id)?;
    let version = plugin.versions.first()?;
    let manifest = &version.manifest;
    let selection = inspection.select(&plugin.id, &manifest.version);
    Some(Offer {
        source: inspection.origin(),
        official: false,
        plugin_id: plugin.id.clone(),
        name: manifest.name.clone(),
        description: manifest.description.clone(),
        version: manifest.version.clone(),
        icon: manifest.icon.clone(),
        size: selection.as_ref().ok().map(Selection::package_size),
        selection,
    })
}

#[cfg(test)]
mod tests;
