//! Which build this is: version label, commit and build date.
//!
//! The values are compiled in from optional environment variables that a build
//! step sets when it knows them (a tagged release build does). There is no
//! build script and no call to git, so a build from a source archive without a
//! `.git` directory works and reports the crate version with an unknown commit.
//! Cargo records every environment variable read by `option_env!` and rebuilds
//! when its value changes.

/// Shown when a build did not say where it came from.
const UNKNOWN: &str = "unknown";

/// The identity of the running build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BuildInfo {
    /// The release label, such as `0.2.0-rc.2`, or the crate version.
    pub version: String,
    /// The short commit the build was made from, or `unknown`.
    pub commit: String,
    /// The commit date (`YYYY-MM-DD`), or `unknown`.
    pub date: String,
}

/// The identity compiled into this executable.
pub(crate) fn current() -> BuildInfo {
    resolve(
        option_env!("AMONG_BUILD_VERSION"),
        option_env!("AMONG_BUILD_COMMIT"),
        option_env!("AMONG_BUILD_DATE"),
    )
}

/// Combine the optional build variables with their fallbacks.
///
/// A variable that a build step sets to an empty string counts as absent: it
/// means the step had nothing to say, not that the value is empty.
pub(crate) fn resolve(
    version: Option<&str>,
    commit: Option<&str>,
    date: Option<&str>,
) -> BuildInfo {
    let present = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    BuildInfo {
        version: present(version).unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned()),
        commit: present(commit).unwrap_or_else(|| UNKNOWN.to_owned()),
        date: present(date).unwrap_or_else(|| UNKNOWN.to_owned()),
    }
}

/// The name of the operating system, from `os-release` text.
///
/// Falls back to `unknown` when the file is missing or has no `PRETTY_NAME`.
pub(crate) fn system_name(os_release: Option<&str>) -> String {
    os_release
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        })
        .map(|value| value.trim().trim_matches(|c| c == '"' || c == '\'').trim())
        .filter(|value| !value.is_empty())
        .map_or_else(|| UNKNOWN.to_owned(), str::to_owned)
}

/// The operating system of this machine.
pub(crate) fn system() -> String {
    let text = std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .ok();
    system_name(text.as_deref())
}

/// Plain text for a bug report: version, commit, build date and system.
pub(crate) fn report_text(info: &BuildInfo, system: &str) -> String {
    format!(
        "Among Terminal {}\nCommit: {}\nBuilt: {}\nSystem: {}\n",
        info.version, info.commit, info.date, system
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_variables_fall_back_to_the_crate_version_and_unknown() {
        let info = resolve(None, None, None);
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.commit, "unknown");
        assert_eq!(info.date, "unknown");
    }

    #[test]
    fn a_release_candidate_label_replaces_the_crate_version() {
        let info = resolve(Some("0.2.0-rc.2"), Some("723e04b"), Some("2026-11-05"));
        assert_eq!(info.version, "0.2.0-rc.2");
        assert_eq!(info.commit, "723e04b");
        assert_eq!(info.date, "2026-11-05");
    }

    #[test]
    fn a_variable_set_to_nothing_is_treated_as_absent() {
        // `option_env!` yields `Some("")` for a variable that is exported empty.
        let info = resolve(Some(""), Some("  "), Some(""));
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.commit, "unknown");
        assert_eq!(info.date, "unknown");
    }

    #[test]
    fn each_variable_falls_back_on_its_own() {
        let info = resolve(None, Some("abcdef0"), None);
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.commit, "abcdef0");
        assert_eq!(info.date, "unknown");
    }

    #[test]
    fn the_system_name_is_read_from_the_pretty_name_line() {
        let text =
            "NAME=\"Debian GNU/Linux\"\nPRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\nID=debian\n";
        assert_eq!(system_name(Some(text)), "Debian GNU/Linux 13 (trixie)");
        assert_eq!(system_name(Some("PRETTY_NAME=Arch Linux\n")), "Arch Linux");
        assert_eq!(system_name(Some("PRETTY_NAME='Fedora 44'\n")), "Fedora 44");
    }

    #[test]
    fn an_unreadable_or_incomplete_os_release_gives_unknown() {
        assert_eq!(system_name(None), "unknown");
        assert_eq!(system_name(Some("NAME=Debian\n")), "unknown");
        assert_eq!(system_name(Some("PRETTY_NAME=\"\"\n")), "unknown");
    }

    #[test]
    fn the_report_text_names_version_commit_date_and_system() {
        let info = resolve(Some("0.2.0-rc.2"), Some("723e04b"), Some("2026-11-05"));
        assert_eq!(
            report_text(&info, "Debian GNU/Linux 13 (trixie)"),
            "Among Terminal 0.2.0-rc.2\nCommit: 723e04b\nBuilt: 2026-11-05\nSystem: Debian GNU/Linux 13 (trixie)\n"
        );
    }
}
