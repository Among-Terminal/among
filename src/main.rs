//! among — a GNOME terminal multiplexer with a pluggable shell-data pipeline.
//!
//! Terminal emulation is delegated to VTE (`libvte-2.91-gtk4`) through the
//! official GNOME World `vte4` crate, which is the same widget GNOME Terminal
//! is built on. This crate owns the pane tree, workspace lifecycle, key
//! routing, and the plugin pipeline.

mod app;
mod context_menu;
mod packaged;
mod pane;
mod plugin;
mod prefs;
mod session;
mod settings;
mod shortcuts;
mod size;
mod slots;
mod tree;
mod workspace;

use gtk::prelude::*;
use gtk::{gio, glib};
// `watch_child` lives on the manual terminal trait, which the prelude exports.
use vte::prelude::*;

fn main() -> glib::ExitCode {
    if let Err(error) = packaged::initialize()
        .and_then(|()| initialize_snap_schemas())
        .and_then(|()| initialize_snap_images())
    {
        eprintln!("among: {error}");
        return glib::ExitCode::FAILURE;
    }
    // An action named in the environment is run once the window is up. This
    // goes through the environment rather than a command-line flag because
    // GApplication parses and rejects any option it does not recognise, which
    // would make a custom flag unusable.
    let requested: Option<crate::shortcuts::Action> = std::env::var("AMONG_ACTION")
        .ok()
        .as_deref()
        .and_then(crate::shortcuts::parse_action);
    if std::env::var_os("AMONG_ACTION").is_some() && requested.is_none() {
        eprintln!("among: unknown action name in AMONG_ACTION");
    }
    // Adw must be initialised before any widget is built, otherwise styled
    // widgets panic on missing style manager properties.
    let app = adw::Application::builder()
        .application_id("com.github.among-terminal")
        .resource_base_path("/com/github/among-terminal")
        .build();

    app.connect_activate(move |app| app::build(app, requested));
    app.run()
}

fn initialize_snap_schemas() -> Result<(), String> {
    let Some(root) = packaged::root() else {
        return Ok(());
    };
    let schemas = root.join("usr/share/glib-2.0/schemas");
    if !schemas.join("gschemas.compiled").is_file() {
        return Err("Packaged payload is missing compiled GSettings schemas".to_string());
    }
    // GLib caches its default source. Initialize it before GTK or worker threads,
    // then restore the caller's environment so host shells/plugins do not inherit
    // an application-private schema path. Snap stage-packages run no postinst.
    let previous = std::env::var_os("GSETTINGS_SCHEMA_DIR");
    std::env::set_var("GSETTINGS_SCHEMA_DIR", schemas);
    let source = gio::SettingsSchemaSource::default();
    match previous {
        Some(value) => std::env::set_var("GSETTINGS_SCHEMA_DIR", value),
        None => std::env::remove_var("GSETTINGS_SCHEMA_DIR"),
    }
    if source
        .and_then(|source| source.lookup("org.gnome.desktop.interface", true))
        .is_none()
    {
        return Err("Snap GSettings schemas could not be initialized".to_string());
    }
    Ok(())
}

fn snap_image_modules(root: &std::path::Path, arch: &str) -> Result<std::path::PathBuf, String> {
    let triplet = match arch {
        "x86_64" => "x86_64-linux-gnu",
        "aarch64" => "aarch64-linux-gnu",
        _ => {
            return Err(format!(
                "Unsupported Snap image-loader architecture: {arch}"
            ))
        }
    };
    Ok(root.join(format!("usr/lib/{triplet}/gdk-pixbuf-2.0/2.10.0")))
}

#[test]
fn snap_image_modules_follow_binary_architecture() {
    let root = std::path::Path::new("/snap/among/current");
    for (arch, triplet) in [
        ("x86_64", "x86_64-linux-gnu"),
        ("aarch64", "aarch64-linux-gnu"),
    ] {
        assert_eq!(
            snap_image_modules(root, arch).unwrap(),
            root.join(format!("usr/lib/{triplet}/gdk-pixbuf-2.0/2.10.0"))
        );
    }
    assert!(snap_image_modules(root, "unsupported").is_err());
}

fn initialize_snap_images() -> Result<(), String> {
    let Some(root) = packaged::root() else {
        return Ok(());
    };
    let mime = root.join("usr/share/mime");
    if !mime.join("mime.cache").is_file() {
        return Err("Snap payload is missing compiled MIME data".to_string());
    }
    let mime = mime.to_str().ok_or("Snap MIME directory is not UTF-8")?;
    let mut directories = gio::content_type_get_mime_dirs();
    directories.insert(0, mime.into());
    let references: Vec<&str> = directories.iter().map(|path| path.as_str()).collect();
    gio::content_type_set_mime_dirs(&references);
    // GTK caches SVG support during initialization. Register before constructing
    // AdwApplication, not at window creation, and do not export loader/XDG paths
    // into the caller's host shells or external plugin processes.
    let native_modules = snap_image_modules(root, std::env::consts::ARCH)?;
    let relocated_modules = root.join("usr/lib/gdk-pixbuf-2.0/2.10.0");
    let modules = if native_modules.join("loaders.cache").is_file() {
        native_modules
    } else if relocated_modules.join("loaders.cache").is_file() {
        relocated_modules
    } else {
        return Err("Packaged payload is missing GdkPixbuf loaders.cache".into());
    };
    let modules = modules
        .to_str()
        .ok_or("Snap image-loader directory is not UTF-8")?;
    gdk_pixbuf::Pixbuf::init_modules(modules)
        .map_err(|error| format!("Snap image loaders could not be initialized: {error}"))?;
    if std::env::var_os("AMONG_SELFCHECK").is_some() {
        for (directory, name) in [
            ("categories", "preferences-system-symbolic"),
            ("actions", "sidebar-show-symbolic"),
            ("actions", "tab-new-symbolic"),
        ] {
            let path = root.join(format!(
                "usr/share/icons/Adwaita/symbolic/{directory}/{name}.svg"
            ));
            gdk_pixbuf::Pixbuf::from_file_at_scale(path, 16, 16, true).map_err(|error| {
                format!("Snap header icon {name} could not be decoded: {error}")
            })?;
            eprintln!("among: header_icon_decode={name}:true");
        }
    }
    Ok(())
}

/// Resolve the user's login shell from the environment, falling back to `bash`.
pub fn user_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string())
}

/// Allocate a PTY. The shell is started separately, once the terminal that will
/// display it exists.
pub fn new_pty() -> Result<vte::Pty, glib::Error> {
    vte::Pty::new_sync(vte::PtyFlags::DEFAULT, None::<&gio::Cancellable>)
}

/// Start the user's shell on a PTY and tell `terminal` to watch it.
///
/// Watching is what makes the terminal report the child's exit. VTE does not
/// infer it from a PTY that was handed to it, so without this call a shell that
/// exits is never noticed — the pane stays open and the exit is never reported.
///
/// The child is started only after the terminal exists because the pid is
/// delivered to the spawn callback, which runs in the parent process.
pub type SpawnCallback = std::rc::Rc<dyn Fn(Result<Option<String>, String>)>;

pub fn spawn_shell(
    pty: &vte::Pty,
    terminal: &vte::Terminal,
    cwd: Option<String>,
    retry: bool,
    done: SpawnCallback,
) {
    let watched = terminal.clone();
    let retry_pty = pty.clone();
    let initial = cwd.clone();
    pty.spawn_async(
        cwd.as_deref(),
        &[&user_shell()],
        &[],
        glib::SpawnFlags::DEFAULT,
        || {},
        -1,
        None::<&gio::Cancellable>,
        move |res| match res {
            Ok(pid) => {
                watched.watch_child(pid);
                let actual = std::fs::read_link(format!("/proc/{}/cwd", pid.0))
                    .ok()
                    .and_then(|path| path.to_str().map(str::to_owned));
                done(Ok(actual));
            }
            Err(err) if retry => {
                eprintln!("among: shell launch failed at saved directory, trying fallback: {err}");
                let (fallback, _) = session::initial_directory(initial.as_deref());
                spawn_shell(&retry_pty, &watched, fallback, false, done);
            }
            Err(err) => done(Err(format!("failed to start shell: {err}"))),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an isolated graphical session"]
    fn shell_spawn_reports_real_directory_after_native_fallback() {
        adw::init().unwrap();
        let pty = new_pty().unwrap();
        let terminal = vte::Terminal::builder().pty(&pty).build();
        let result = std::rc::Rc::new(std::cell::RefCell::new(None));
        let received = result.clone();
        let missing = format!("/tmp/opencode/no-such-directory-{}", std::process::id());
        let expected = crate::session::initial_directory(Some(&missing)).0;
        let startup = std::env::current_dir()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        spawn_shell(
            &pty,
            &terminal,
            Some(missing),
            true,
            std::rc::Rc::new(move |value| {
                assert!(received.borrow().is_none(), "completion delivered once");
                *received.borrow_mut() = Some(value);
            }),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while result.borrow().is_none() && std::time::Instant::now() < deadline {
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let actual = result
            .borrow()
            .clone()
            .expect("spawn callback arrived")
            .unwrap();
        assert!(
            actual == expected || actual.as_deref() == Some(startup.as_str()),
            "actual native fallback cwd: {actual:?}"
        );
        println!("PASS unavailable spawn directory handled with one completion and actual native fallback cwd");

        let shell = std::env::var_os("SHELL");
        std::env::set_var("SHELL", "/tmp/opencode/no-such-shell-executable");
        let pty = new_pty().unwrap();
        let terminal = vte::Terminal::builder().pty(&pty).build();
        let failure = std::rc::Rc::new(std::cell::RefCell::new(None));
        let received = failure.clone();
        spawn_shell(
            &pty,
            &terminal,
            None,
            true,
            std::rc::Rc::new(move |value| {
                assert!(received.borrow().is_none());
                *received.borrow_mut() = Some(value);
            }),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while failure.borrow().is_none() && std::time::Instant::now() < deadline {
            while glib::MainContext::default().iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if let Some(shell) = shell {
            std::env::set_var("SHELL", shell);
        } else {
            std::env::remove_var("SHELL");
        }
        assert!(
            failure.borrow().as_ref().unwrap().is_err(),
            "a launch failure completes after one bounded retry"
        );
    }
}
