//! Version order for deciding whether a release is an update.
//!
//! Manifests carry a free-text version, so an order exists only for versions
//! written as Semantic Versioning 2.0.0. Anything else has no defined order:
//! the caller refuses it rather than guessing which of two versions is newer.

use std::cmp::Ordering;

/// A Semantic Versioning 2.0.0 version. Build metadata is validated and then
/// dropped because it takes no part in precedence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemVer {
    core: [u64; 3],
    pre_release: Vec<Identifier>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Identifier {
    Numeric(u64),
    Text(String),
}

impl Ord for Identifier {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Numeric(left), Self::Numeric(right)) => left.cmp(right),
            // Numeric identifiers sort below alphanumeric ones.
            (Self::Numeric(_), Self::Text(_)) => Ordering::Less,
            (Self::Text(_), Self::Numeric(_)) => Ordering::Greater,
            (Self::Text(left), Self::Text(right)) => left.as_bytes().cmp(right.as_bytes()),
        }
    }
}

impl PartialOrd for Identifier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl SemVer {
    /// Parses exactly the grammar of the specification: no `v` prefix, no
    /// missing components and no leading zeros in numeric parts.
    pub fn parse(text: &str) -> Option<Self> {
        let (without_build, build) = match text.split_once('+') {
            Some((head, build)) => (head, Some(build)),
            None => (text, None),
        };
        if let Some(build) = build {
            if !build.split('.').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            }) {
                return None;
            }
        }
        let (core, pre) = match without_build.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (without_build, None),
        };
        let mut numbers = core.split('.').map(numeric);
        let core = [numbers.next()??, numbers.next()??, numbers.next()??];
        if numbers.next().is_some() {
            return None;
        }
        let pre_release = match pre {
            Some(pre) => pre.split('.').map(identifier).collect::<Option<Vec<_>>>()?,
            None => Vec::new(),
        };
        Some(Self { core, pre_release })
    }
}

/// A decimal number without a sign and without a leading zero.
fn numeric(part: &str) -> Option<u64> {
    if part.is_empty()
        || !part.bytes().all(|byte| byte.is_ascii_digit())
        || (part.len() > 1 && part.starts_with('0'))
    {
        return None;
    }
    part.parse().ok()
}

fn identifier(part: &str) -> Option<Identifier> {
    if part.is_empty()
        || !part
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return None;
    }
    if part.bytes().all(|byte| byte.is_ascii_digit()) {
        return numeric(part).map(Identifier::Numeric);
    }
    Some(Identifier::Text(part.to_owned()))
}

impl Ord for SemVer {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| {
            match (&self.pre_release[..], &other.pre_release[..]) {
                ([], []) => Ordering::Equal,
                // A version without a pre-release part is the later one.
                ([], _) => Ordering::Greater,
                (_, []) => Ordering::Less,
                // Slices order element by element and, on a shared prefix, the
                // shorter list first, which is the specification's rule.
                (left, right) => left.cmp(right),
            }
        })
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> SemVer {
        SemVer::parse(text).unwrap_or_else(|| panic!("{text} is valid"))
    }

    #[test]
    fn precedence_follows_the_published_ordering_chain() {
        // The chain given in the specification's precedence section.
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
            "2.0.0",
            "2.1.0",
            "2.1.1",
        ];
        for (index, lower) in chain.iter().enumerate() {
            for higher in &chain[index + 1..] {
                assert!(version(lower) < version(higher), "{lower} < {higher}");
                assert!(version(higher) > version(lower), "{higher} > {lower}");
            }
        }
        // Numbers compare as numbers, not as text.
        assert!(version("1.9.0") < version("1.10.0"));
        assert!(version("1.0.0-2") < version("1.0.0-10"));
    }

    #[test]
    fn build_metadata_does_not_change_precedence() {
        assert_eq!(version("1.0.0+a").cmp(&version("1.0.0+b")), Ordering::Equal);
        assert_eq!(version("1.0.0-rc.1+x"), version("1.0.0-rc.1"));
    }

    #[test]
    fn anything_outside_the_grammar_has_no_order() {
        for text in [
            "",
            "1",
            "1.0",
            "2026.10",
            "v1.0.0",
            "1.0.0.0",
            "01.0.0",
            "1.00.0",
            "1.0.0-",
            "1.0.0-01",
            "1.0.0-a..b",
            "1.0.0+",
            "1.0.0+a..b",
            "1.0.0+é",
            "-1.0.0",
            "+1.0.0",
            "1.0.-0",
            " 1.0.0",
            "1.0.0 ",
            "18446744073709551616.0.0",
            "1.0.0-18446744073709551616",
        ] {
            assert!(SemVer::parse(text).is_none(), "{text:?}");
        }
    }
}
