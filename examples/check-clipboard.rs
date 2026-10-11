use gtk::{gdk, gio, glib, prelude::*};
use std::{cell::RefCell, path::PathBuf, rc::Rc, time::Duration};

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let [runner_home, mode, rest @ ..] = args.as_slice() else {
        return Err("usage: check-clipboard OWNED_DISPLAY_HOME read|binary|set [TEXT]".into());
    };
    let home = PathBuf::from(runner_home);
    let base = home.parent().ok_or("display home has no parent")?;
    if home.file_name() != Some(std::ffi::OsStr::new("home"))
        || !base
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("among-rust-display-"))
    {
        return Err("clipboard fixture requires owned display evidence".into());
    }
    let display = std::env::var("DISPLAY")?;
    let mut owned = false;
    for marker in ["xvfb-display.txt", "xephyr-display.txt"] {
        let path = base.join(marker);
        if path.exists()
            && !path.symlink_metadata()?.file_type().is_symlink()
            && std::fs::read_to_string(path)? == display
        {
            owned = true;
        }
    }
    if !owned {
        return Err("clipboard fixture DISPLAY does not match owned evidence".into());
    }
    gtk::init()?;
    let clipboard = gdk::Display::default()
        .ok_or("owned display unavailable")?
        .clipboard();
    let main_loop = glib::MainLoop::new(None, false);
    if mode == "read" && rest.is_empty() {
        let result = Rc::new(RefCell::new(None));
        let received = result.clone();
        let finished = main_loop.clone();
        clipboard.read_text_async(None::<&gio::Cancellable>, move |value| {
            *received.borrow_mut() =
                Some(value.map(|text| text.map(|value| value.to_string()).unwrap_or_default()));
            finished.quit();
        });
        let deadline = main_loop.clone();
        glib::timeout_add_local_once(Duration::from_secs(8), move || deadline.quit());
        main_loop.run();
        let value = result
            .borrow_mut()
            .take()
            .ok_or("clipboard read timed out")??;
        use std::io::Write;
        std::io::stdout().write_all(value.as_bytes())?;
    } else {
        let provider = match (mode.as_str(), rest) {
            ("binary", []) => gdk::ContentProvider::for_bytes(
                "application/octet-stream",
                &glib::Bytes::from_static(b"binary"),
            ),
            ("set", [text]) => gdk::ContentProvider::for_value(&text.to_value()),
            _ => return Err("invalid clipboard operation".into()),
        };
        clipboard.set_content(Some(&provider))?;
        let deadline = main_loop.clone();
        glib::timeout_add_local_once(Duration::from_secs(120), move || deadline.quit());
        main_loop.run();
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("clipboard fixture: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
