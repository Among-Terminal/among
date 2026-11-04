//! the one directory a confined plugin may write to. it is created component
//! by component through directory descriptors, so a symbolic link or a
//! foreign owner anywhere on the way refuses the start instead of redirecting
//! the plugin's writes.

use std::ffi::CString;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

/// A checked data directory: the open descriptor Landlock is given and the
/// path the plugin is told about.
pub struct DataDir {
    pub directory: File,
    pub path: PathBuf,
}

/// The directory below which per-plugin data lives: `$XDG_DATA_HOME/among`,
/// with `$HOME/.local/share` as the fallback the managed store also uses.
pub fn data_home() -> Option<PathBuf> {
    data_home_from(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}

fn data_home_from(
    xdg_data_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    xdg_data_home
        .map(PathBuf::from)
        // the XDG specification treats an empty or relative value as invalid.
        .filter(|path| path.is_absolute())
        .or_else(|| home.map(|home| PathBuf::from(home).join(".local/share")))
}

const TOP: &str = "among";
const BRANCH: &str = "plugin-data";

/// Why the data directory cannot be used. The text names the offending path
/// and what to change, because it is shown to the user.
#[derive(Debug)]
pub struct Problem(pub String);

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn problem(text: &str) -> Problem {
    Problem(text.to_string())
}

fn component(name: &str) -> Result<CString, Problem> {
    let plain = !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if !plain {
        return Err(problem("plugin data directory name is not a plain name"));
    }
    CString::new(name).map_err(|_| problem("plugin data directory name is not a plain name"))
}

/// Opens `name` below `parent` without following a link. `shown` is the path
/// the user knows the directory by, used in the refusal.
fn open_directory(parent: &File, name: &CString, shown: &Path) -> Result<File, Problem> {
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        let error = std::io::Error::last_os_error();
        // O_NOFOLLOW on a link, or O_DIRECTORY on a file, both mean the entry is
        // not a plain directory.
        let link = matches!(
            error.raw_os_error(),
            Some(libc::ELOOP) | Some(libc::ENOTDIR)
        );
        return Err(Problem(if link {
            format!(
                "plugin data directory path contains a link or a non-directory at {} \
                 (replace it by a real directory, or start Among with XDG_DATA_HOME \
                 pointing at a path without links)",
                shown.display()
            )
        } else {
            format!(
                "plugin data directory path has an unavailable directory at {}: {error}",
                shown.display()
            )
        }));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Creates the directory when it is missing; true when this call created it.
fn create_if_missing(parent: &File, name: &CString) -> Result<bool, Problem> {
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } == 0 {
        return Ok(true);
    }
    if std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
        return Err(problem("cannot create the plugin data directory"));
    }
    Ok(false)
}

/// A directory the user owns; the plugin's own directory must also be private.
///
/// The directories above the leaf may be group-writable: with the usual
/// desktop umask of `002` the data home and `among` are created that way, and
/// refusing them would refuse every plugin. A replacement made by someone with
/// write access is caught by the owner check on each descriptor that was
/// opened without following links.
fn check_owned(directory: &File, leaf: bool, shown: &Path) -> Result<(), Problem> {
    let metadata = directory
        .metadata()
        .map_err(|_| problem("cannot inspect the plugin data directory"))?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(Problem(format!(
            "plugin data directory {} is not a directory owned by the current user",
            shown.display()
        )));
    }
    if leaf && metadata.mode() & 0o7777 != 0o700 {
        return Err(Problem(format!(
            "plugin data directory {} has permissions {:o}, not the required 700",
            shown.display(),
            metadata.mode() & 0o7777
        )));
    }
    Ok(())
}

/// Opens or creates `<data_home>/among/plugin-data/<namespace>/<plugin_id>`.
///
/// Links are refused everywhere on the path. From the data home down every
/// directory must be owned by the user, and the leaf must be exactly mode
/// `0700`.
pub fn open(data_home: &Path, namespace: &str, plugin_id: &str) -> Result<DataDir, Problem> {
    if !data_home.is_absolute() {
        return Err(problem("plugin data home must be an absolute path"));
    }
    let mut current = File::open("/").map_err(|_| problem("cannot open the filesystem root"))?;
    let mut path = PathBuf::from("/");
    for part in data_home.components() {
        match part {
            Component::RootDir => continue,
            Component::Normal(name) => {
                let name = name
                    .to_str()
                    .ok_or(problem("plugin data home is not UTF-8"))?;
                let name_c = CString::new(name)
                    .map_err(|_| problem("plugin data home contains a NUL byte"))?;
                path.push(name);
                current = open_directory(&current, &name_c, &path)?;
            }
            _ => return Err(problem("plugin data home is not a normalised path")),
        }
    }
    check_owned(&current, false, &path)?;
    let names = [TOP, BRANCH, namespace, plugin_id];
    // every name is judged before the first directory is created.
    let encoded = names
        .iter()
        .map(|name| component(name))
        .collect::<Result<Vec<_>, _>>()?;
    for (index, (name, name_c)) in names.iter().zip(&encoded).enumerate() {
        path.push(name);
        let created = create_if_missing(&current, name_c)?;
        current = open_directory(&current, name_c, &path)?;
        let leaf = index == names.len() - 1;
        // mkdir applies the umask, which can only narrow the mode but could
        // leave a directory the owner cannot use; a directory made here is set
        // to exactly 0700 on its descriptor whatever the umask was.
        if created && unsafe { libc::fchmod(current.as_raw_fd(), 0o700) } != 0 {
            return Err(problem("cannot set the plugin data directory mode"));
        }
        check_owned(&current, leaf, &path)?;
    }
    Ok(DataDir {
        directory: current,
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let root = std::env::var_os("AMONG_CHECK_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir)
                .join(format!("among-data-dir-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(root)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_narrow_umask_still_yields_exactly_0700_on_a_new_leaf() {
        const MARKER: &str = "AMONG_NARROW_UMASK_PROCESS";
        // umask is process-wide, so this runs in a process of its own.
        if std::env::var_os(MARKER).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "plugin::sandbox::data_dir::tests::a_narrow_umask_still_yields_exactly_0700_on_a_new_leaf",
                    "--exact",
                    "--nocapture",
                ])
                .env(MARKER, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let scratch = Scratch::new("narrow-umask");
        // 0o377 removes read and write from the owner as well.
        unsafe { libc::umask(0o377) };
        let opened = open(&scratch.0, "12", "clock").unwrap();
        assert_eq!(
            std::fs::metadata(&opened.path).unwrap().mode() & 0o7777,
            0o700
        );
        // and the directory can be reopened afterwards.
        assert!(open(&scratch.0, "12", "clock").is_ok());
    }

    #[test]
    fn the_data_home_prefers_xdg_then_home_and_is_absent_without_both() {
        use std::ffi::OsString;
        let some = |text: &str| Some(OsString::from(text));
        assert_eq!(
            data_home_from(some("/xdg"), some("/home/u")),
            Some(PathBuf::from("/xdg"))
        );
        // an empty value counts as unset, as the managed store treats it.
        assert_eq!(
            data_home_from(some(""), some("/home/u")),
            Some(PathBuf::from("/home/u/.local/share"))
        );
        assert_eq!(
            data_home_from(None, some("/home/u")),
            Some(PathBuf::from("/home/u/.local/share"))
        );
        // a relative value is invalid and ignored.
        assert_eq!(
            data_home_from(some("relative/share"), some("/home/u")),
            Some(PathBuf::from("/home/u/.local/share"))
        );
        assert_eq!(data_home_from(some("relative"), None), None);
        assert_eq!(data_home_from(None, None), None);
        assert_eq!(data_home_from(some(""), None), None);
    }

    #[test]
    fn creates_a_private_leaf_and_reopens_it_with_its_content() {
        let scratch = Scratch::new("create");
        let first = open(&scratch.0, "12", "clock").unwrap();
        assert_eq!(first.path, scratch.0.join("among/plugin-data/12/clock"));
        let mode = std::fs::metadata(&first.path).unwrap().mode() & 0o7777;
        assert_eq!(mode, 0o700);
        std::fs::write(first.path.join("state"), b"kept").unwrap();
        let second = open(&scratch.0, "12", "clock").unwrap();
        assert_eq!(std::fs::read(second.path.join("state")).unwrap(), b"kept");
    }

    #[test]
    fn development_installs_use_their_own_namespace() {
        let scratch = Scratch::new("development");
        let opened = open(&scratch.0, "development", "mine").unwrap();
        assert_eq!(
            opened.path,
            scratch.0.join("among/plugin-data/development/mine")
        );
    }

    #[test]
    fn a_link_at_any_level_refuses_the_start() {
        for level in 0..5 {
            let scratch = Scratch::new(&format!("link{level}"));
            let elsewhere = scratch.0.join("elsewhere");
            std::fs::create_dir(&elsewhere).unwrap();
            std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o700)).unwrap();
            // lay the real path out, then replace one directory by a link.
            let base = scratch.0.join("home");
            std::fs::create_dir(&base).unwrap();
            std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
            open(&base, "12", "clock").unwrap();
            let names = [
                base.clone(),
                base.join("among"),
                base.join("among/plugin-data"),
                base.join("among/plugin-data/12"),
                base.join("among/plugin-data/12/clock"),
            ];
            let victim = &names[level];
            let moved = scratch.0.join("moved");
            std::fs::rename(victim, &moved).unwrap();
            symlink(&moved, victim).unwrap();
            let result = open(&base, "12", "clock");
            assert!(result.is_err(), "link at level {level} was followed");
            let reason = result.err().unwrap().0;
            assert!(reason.contains("link"), "level {level}: {reason}");
            // the reason names the component that is a link, so the user can fix it.
            assert!(
                reason.contains(&format!("at {}", victim.display())),
                "level {level} names {}: {reason}",
                victim.display()
            );
            assert!(reason.contains("XDG_DATA_HOME"), "{reason}");
        }
    }

    #[test]
    fn a_leaf_wider_than_owner_only_is_refused_not_repaired() {
        let scratch = Scratch::new("wide");
        let first = open(&scratch.0, "12", "clock").unwrap();
        std::fs::set_permissions(&first.path, std::fs::Permissions::from_mode(0o750)).unwrap();
        let error = open(&scratch.0, "12", "clock").err().unwrap();
        assert!(error.0.contains("permissions"));
        assert_eq!(
            std::fs::metadata(&first.path).unwrap().mode() & 0o7777,
            0o750,
            "the refusal does not rewrite modes"
        );
    }

    #[test]
    fn directories_above_the_leaf_may_be_group_writable_as_a_002_umask_makes_them() {
        let scratch = Scratch::new("group-writable-parents");
        open(&scratch.0, "12", "clock").unwrap();
        // the data home and every directory above the leaf as `umask 002` and
        // other parts of the application create them.
        for directory in [
            scratch.0.clone(),
            scratch.0.join("among"),
            scratch.0.join("among/plugin-data"),
            scratch.0.join("among/plugin-data/12"),
        ] {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o775)).unwrap();
        }
        let opened = open(&scratch.0, "12", "clock").unwrap();
        assert_eq!(
            std::fs::metadata(&opened.path).unwrap().mode() & 0o7777,
            0o700
        );
    }

    #[test]
    fn names_that_are_not_plain_names_are_refused_before_anything_is_created() {
        let scratch = Scratch::new("names");
        for (namespace, plugin) in [
            ("..", "clock"),
            ("12", ".."),
            ("12", "a/b"),
            ("12", ""),
            ("a b", "clock"),
        ] {
            assert!(open(&scratch.0, namespace, plugin).is_err());
        }
        assert!(!scratch.0.join("among/plugin-data/12").exists());
        assert!(open(Path::new("relative"), "12", "clock").is_err());
    }

    #[test]
    fn a_directory_owned_by_another_user_is_refused() {
        // a directory this user does not own: the filesystem root belongs to root,
        // so using it as the data home must refuse. When the tests run as root
        // the check cannot observe a foreign owner and says so by skipping.
        if unsafe { libc::geteuid() } == 0 {
            eprintln!("NOT VERIFIED: running as root, no foreign-owned directory to observe");
            return;
        }
        let error = open(Path::new("/"), "12", "clock").err().unwrap();
        assert!(error.0.contains("owned by the current user"));
    }

    #[test]
    fn a_component_swapped_for_a_link_during_opening_is_never_followed() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let scratch = Scratch::new("race");
        let base = scratch.0.join("home");
        std::fs::create_dir(&base).unwrap();
        std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700)).unwrap();
        let outside = scratch.0.join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o700)).unwrap();
        open(&base, "12", "clock").unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let swapper = {
            let stop = stop.clone();
            let target = base.join("among/plugin-data/12");
            let parked = scratch.0.join("parked");
            let outside = outside.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    // alternate the real directory and a link to the outside.
                    if std::fs::rename(&target, &parked).is_ok() {
                        let _ = symlink(&outside, &target);
                        let _ = std::fs::remove_file(&target);
                        let _ = std::fs::rename(&parked, &target);
                    }
                }
            })
        };
        let real = base.join("among/plugin-data/12/clock");
        let parked = scratch.0.join("parked/clock");
        let mut opened = 0;
        for _ in 0..3000 {
            if let Ok(directory) = open(&base, "12", "clock") {
                opened += 1;
                // the swapper may have moved the real directory after it was
                // opened; it is never anything but the real leaf.
                let fd_path = format!("/proc/self/fd/{}", directory.directory.as_raw_fd());
                let resolved = std::fs::read_link(fd_path).unwrap();
                assert!(
                    resolved == real || resolved == parked,
                    "descriptor resolves outside the data root: {resolved:?}"
                );
            }
        }
        stop.store(true, Ordering::Relaxed);
        swapper.join().unwrap();
        assert!(
            !outside.join("clock").exists(),
            "a directory was created through a link"
        );
        eprintln!("race: {opened} of 3000 opens succeeded, none through a link");
    }
}
