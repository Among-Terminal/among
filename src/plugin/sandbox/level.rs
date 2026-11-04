//! how much protection a start gets, and what the host does about it. the
//! decision is a pure function of a probe result and the user's consent, so
//! every row of it is tested without a particular kernel.

use super::policy::FULL_LANDLOCK_ABI;
use super::status::{Enforcement, Report};

/// The protection a plugin process runs under. A stored consent covers its own
/// level and any stronger one (see `rank`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protection {
    /// Landlock ABI 3 or newer plus the seccomp filter.
    Full { abi: u32 },
    /// Landlock below ABI 3 plus the seccomp filter; `abi` is 1 or 2.
    Reduced { abi: u32 },
    /// The seccomp filter only: the kernel has no usable Landlock.
    SeccompOnly,
    /// The seccomp filter could not be loaded. Never runs a plugin.
    Unprotected,
}

impl Protection {
    /// Strength used for ordering: higher is stronger.
    fn rank(self) -> u32 {
        match self {
            Self::Full { abi } => 1000 + abi,
            Self::Reduced { abi } => 100 + abi,
            Self::SeccompOnly => 10,
            Self::Unprotected => 0,
        }
    }

    /// Whether this level is at least as strong as `required`.
    pub fn satisfies(self, required: Self) -> bool {
        self.rank() >= required.rank()
    }

    pub fn is_full(self) -> bool {
        matches!(self, Self::Full { .. })
    }

    /// The text kept in the local store as consent and shown to the user.
    pub fn label(self) -> String {
        match self {
            Self::Full { abi } => format!("full (Landlock ABI {abi} and seccomp)"),
            Self::Reduced { abi } => {
                format!("reduced (Landlock ABI {abi}, below the required {FULL_LANDLOCK_ABI}, and seccomp)")
            }
            Self::SeccompOnly => "reduced (seccomp only, no Landlock)".into(),
            Self::Unprotected => "none (seccomp could not be loaded)".into(),
        }
    }

    /// The stable identifier stored with a consent.
    pub fn token(self) -> String {
        match self {
            Self::Full { abi } => format!("full:{abi}"),
            Self::Reduced { abi } => format!("reduced:{abi}"),
            Self::SeccompOnly => "seccomp-only".into(),
            Self::Unprotected => "none".into(),
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        let level = if let Some(abi) = token.strip_prefix("full:") {
            Self::Full {
                abi: abi.parse().ok().filter(|abi| *abi >= FULL_LANDLOCK_ABI)?,
            }
        } else if let Some(abi) = token.strip_prefix("reduced:") {
            Self::Reduced {
                abi: abi
                    .parse()
                    .ok()
                    .filter(|abi| (1..FULL_LANDLOCK_ABI).contains(abi))?,
            }
        } else {
            match token {
                "seccomp-only" => Self::SeccompOnly,
                "none" => Self::Unprotected,
                _ => return None,
            }
        };
        Some(level)
    }

    /// The protection implied by what a child reported.
    pub fn of_report(report: &Report) -> Self {
        if !report.seccomp_loaded || !report.no_new_privs || report.failed as u8 != 0 {
            return Self::Unprotected;
        }
        let abi = u32::from(report.landlock_abi);
        match report.enforcement {
            Enforcement::None => Self::SeccompOnly,
            Enforcement::Full if abi >= FULL_LANDLOCK_ABI => Self::Full { abi },
            // a ruleset that enforced fewer rights than asked for is not the
            // protection the full level promises, whatever the kernel's ABI.
            Enforcement::Partial | Enforcement::Full => Self::Reduced {
                abi: abi.clamp(1, FULL_LANDLOCK_ABI - 1),
            },
        }
    }
}

/// What the running kernel offers, observed before a start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Probe {
    /// Landlock ABI, or `None` when Landlock is absent or disabled.
    pub landlock_abi: Option<u32>,
    /// Whether the kernel accepts a seccomp filter at all.
    pub seccomp: bool,
}

impl Probe {
    /// The best protection a start can reach on this kernel.
    pub fn best(self) -> Protection {
        if !self.seccomp {
            return Protection::Unprotected;
        }
        match self.landlock_abi {
            Some(abi) if abi >= FULL_LANDLOCK_ABI => Protection::Full { abi },
            Some(abi) if abi >= 1 => Protection::Reduced { abi },
            _ => Protection::SeccompOnly,
        }
    }
}

/// Reads the running kernel's offer. Both questions are answered without
/// changing the calling process.
pub fn probe() -> Probe {
    // LANDLOCK_CREATE_RULESET_VERSION asks for the ABI number.
    let version = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            1u32,
        )
    };
    let landlock_abi = (version > 0).then_some(version as u32);
    // SECCOMP_GET_ACTION_AVAIL (2) answers whether a return action exists;
    // success also shows the system call itself is implemented.
    let action: u32 = 0x0005_0000; // SECCOMP_RET_ERRNO
    let seccomp = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            2 as libc::c_ulong,
            0 as libc::c_ulong,
            &action as *const u32,
        )
    } == 0;
    Probe {
        landlock_abi,
        seccomp,
    }
}

/// The host's decision for one start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Start under the best protection the kernel offers; the child must reach
    /// at least `required`.
    Start { required: Protection },
    /// Do not start: seccomp is unavailable, with or without consent.
    Refuse { reason: String },
    /// Do not start until the user consents to the reduced level.
    NeedsConsent { offered: Protection },
}

/// Decides one start from the probe and the level the user consented to.
pub fn decide(probe: Probe, consent: Option<Protection>) -> Decision {
    let best = probe.best();
    if best == Protection::Unprotected {
        return Decision::Refuse {
            reason: "the kernel cannot load a seccomp filter; the plugin is not started".into(),
        };
    }
    if best.is_full() {
        return Decision::Start { required: best };
    }
    match consent {
        Some(consented) if best.satisfies(consented) && consented != Protection::Unprotected => {
            Decision::Start {
                required: consented,
            }
        }
        _ => Decision::NeedsConsent { offered: best },
    }
}

/// The text Preferences shows when a start is refused for lack of consent.
pub fn consent_message(offered: Protection, previously_consented: Option<Protection>) -> String {
    let observed = match offered {
        Protection::Reduced { abi } => format!("the kernel supports only Landlock ABI {abi}"),
        _ => "the kernel has no usable Landlock".to_string(),
    };
    let again = previously_consented.map_or(String::new(), |level| {
        format!(
            " You allowed {}, which this kernel no longer reaches.",
            level.label()
        )
    });
    format!(
        "{observed} (required: ABI {FULL_LANDLOCK_ABI}, kernel 6.2 or newer); the plugin was not started.{again} \
         Allow starting with reduced protection to run it with {}.",
        offered.label()
    )
}

/// What the sandbox restricts at `level`, in the words the access review shows
/// before the user accepts a plugin. The sentence must not promise a file
/// restriction the level does not have, because the user decides on it.
pub fn restrictions_summary(level: Protection) -> &'static str {
    match level {
        Protection::Full { .. } => {
            "it gets no network and no files except its own data directory and the system runtime it needs to start"
        }
        Protection::Reduced { .. } => {
            "it gets no network and cannot read or write files outside its own data directory and the system runtime, except that this kernel's Landlock cannot stop it from emptying (truncating) a file you can write"
        }
        Protection::SeccompOnly => {
            "it gets no network and no control over other processes, but this kernel has no usable Landlock, so its file access is NOT restricted: it can read and write every file your account can"
        }
        Protection::Unprotected => {
            "this kernel cannot load a seccomp filter, so the plugin cannot be started at all"
        }
    }
}

/// The introduction of the access review for a kernel offering `level`.
pub fn review_intro(level: Protection) -> String {
    format!(
        "This executable runs as your user inside a kernel sandbox. On this kernel {}. \
         The access below is what Among sends it and, for system metrics, a few read-only files. \
         Install only code you trust. Required access must be accepted; optional access starts off.",
        restrictions_summary(level)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_access_review_states_what_each_level_really_restricts() {
        let full = review_intro(Protection::Full { abi: 6 });
        assert!(full.contains("no files except its own data directory"));
        assert!(!full.contains("NOT restricted"));

        for abi in [1, 2] {
            let reduced = review_intro(Protection::Reduced { abi });
            assert!(reduced.contains("outside its own data directory"));
            assert!(reduced.contains("truncating"));
            assert!(!reduced.contains("no files except"));
        }

        let seccomp_only = review_intro(Protection::SeccompOnly);
        assert!(seccomp_only.contains("file access is NOT restricted"));
        assert!(seccomp_only.contains("every file your account can"));
        assert!(!seccomp_only.contains("no files except"));
        assert!(!seccomp_only.contains("outside its own data directory"));

        let none = review_intro(Protection::Unprotected);
        assert!(none.contains("cannot be started"));
        assert!(!none.contains("no files except"));
    }

    const fn probe(landlock_abi: Option<u32>, seccomp: bool) -> Probe {
        Probe {
            landlock_abi,
            seccomp,
        }
    }

    #[test]
    fn the_best_level_follows_the_observed_abi_and_seccomp() {
        assert_eq!(probe(Some(6), true).best(), Protection::Full { abi: 6 });
        assert_eq!(probe(Some(3), true).best(), Protection::Full { abi: 3 });
        assert_eq!(probe(Some(2), true).best(), Protection::Reduced { abi: 2 });
        assert_eq!(probe(Some(1), true).best(), Protection::Reduced { abi: 1 });
        assert_eq!(probe(None, true).best(), Protection::SeccompOnly);
        assert_eq!(probe(Some(6), false).best(), Protection::Unprotected);
        assert_eq!(probe(None, false).best(), Protection::Unprotected);
    }

    #[test]
    fn a_full_kernel_starts_without_consent_and_requires_what_it_offers() {
        assert_eq!(
            decide(probe(Some(4), true), None),
            Decision::Start {
                required: Protection::Full { abi: 4 }
            }
        );
    }

    #[test]
    fn an_older_kernel_without_consent_does_not_start() {
        for (landlock, offered) in [
            (Some(2), Protection::Reduced { abi: 2 }),
            (Some(1), Protection::Reduced { abi: 1 }),
            (None, Protection::SeccompOnly),
        ] {
            assert_eq!(
                decide(probe(landlock, true), None),
                Decision::NeedsConsent { offered }
            );
        }
    }

    #[test]
    fn consent_at_the_observed_level_or_weaker_starts_with_that_requirement() {
        assert_eq!(
            decide(probe(Some(2), true), Some(Protection::Reduced { abi: 2 })),
            Decision::Start {
                required: Protection::Reduced { abi: 2 }
            }
        );
        // the user accepted seccomp only; a kernel that now offers more still
        // starts, and the child is held to the consented floor.
        assert_eq!(
            decide(probe(Some(2), true), Some(Protection::SeccompOnly)),
            Decision::Start {
                required: Protection::SeccompOnly
            }
        );
    }

    #[test]
    fn a_level_below_the_consented_one_asks_again() {
        assert_eq!(
            decide(probe(Some(1), true), Some(Protection::Reduced { abi: 2 })),
            Decision::NeedsConsent {
                offered: Protection::Reduced { abi: 1 }
            }
        );
        assert_eq!(
            decide(probe(None, true), Some(Protection::Reduced { abi: 1 })),
            Decision::NeedsConsent {
                offered: Protection::SeccompOnly
            }
        );
    }

    #[test]
    fn without_seccomp_nothing_starts_even_with_every_consent() {
        for consent in [
            None,
            Some(Protection::SeccompOnly),
            Some(Protection::Reduced { abi: 1 }),
            Some(Protection::Unprotected),
            Some(Protection::Full { abi: 3 }),
        ] {
            assert!(matches!(
                decide(probe(Some(6), false), consent),
                Decision::Refuse { .. }
            ));
        }
    }

    #[test]
    fn a_stored_consent_to_nothing_never_authorises_a_start() {
        assert_eq!(
            decide(probe(None, true), Some(Protection::Unprotected)),
            Decision::NeedsConsent {
                offered: Protection::SeccompOnly
            }
        );
    }

    #[test]
    fn the_level_a_child_applied_comes_from_its_own_report() {
        let report = |abi, enforcement, seccomp_loaded| Report {
            no_new_privs: true,
            seccomp_loaded,
            landlock_abi: abi,
            enforcement,
            failed: super::super::status::Step::None,
            errno: 0,
        };
        assert_eq!(
            Protection::of_report(&report(5, Enforcement::Full, true)),
            Protection::Full { abi: 5 }
        );
        assert_eq!(
            Protection::of_report(&report(2, Enforcement::Full, true)),
            Protection::Reduced { abi: 2 }
        );
        assert_eq!(
            Protection::of_report(&report(0, Enforcement::None, true)),
            Protection::SeccompOnly
        );
        assert_eq!(
            Protection::of_report(&report(5, Enforcement::Full, false)),
            Protection::Unprotected
        );
        // partially enforced rights never count as the full level.
        assert_eq!(
            Protection::of_report(&report(4, Enforcement::Partial, true)),
            Protection::Reduced { abi: 2 }
        );
    }

    #[test]
    fn tokens_round_trip_and_reject_values_that_name_no_level() {
        for level in [
            Protection::Full { abi: 3 },
            Protection::Reduced { abi: 1 },
            Protection::SeccompOnly,
            Protection::Unprotected,
        ] {
            assert_eq!(Protection::from_token(&level.token()), Some(level));
        }
        for bad in [
            "",
            "full:2",
            "reduced:3",
            "reduced:0",
            "full:x",
            "seccomp",
            "FULL:3",
        ] {
            assert_eq!(Protection::from_token(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_refusal_text_names_observed_required_and_the_way_to_allow() {
        let text = consent_message(Protection::Reduced { abi: 2 }, None);
        assert_eq!(
            text,
            "the kernel supports only Landlock ABI 2 (required: ABI 3, kernel 6.2 or newer); \
             the plugin was not started. Allow starting with reduced protection to run it with \
             reduced (Landlock ABI 2, below the required 3, and seccomp)."
        );
        let again = consent_message(
            Protection::SeccompOnly,
            Some(Protection::Reduced { abi: 2 }),
        );
        assert!(again.starts_with("the kernel has no usable Landlock (required: ABI 3"));
        assert!(again.contains("You allowed reduced (Landlock ABI 2"));
    }
}
