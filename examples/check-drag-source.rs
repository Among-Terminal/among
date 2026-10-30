// A separate X11 client that offers one foreign payload by native drag.
//
// The pane-move check drags from this window onto Among's sidebar to prove that
// data from another application is never taken for a pane: terminal text, a
// file, and an opaque number in a private format that no GTK type can decode.
// It runs only on a display the check owns, as the clipboard fixture does.
use gtk::{gdk, gio, glib, prelude::*};
use std::path::PathBuf;

const TITLE: &str = "Among foreign drag source";

fn owned_display(runner_home: &str) -> Result<(), Box<dyn std::error::Error>> {
    let home = PathBuf::from(runner_home);
    let base = home.parent().ok_or("display home has no parent")?;
    if home.file_name() != Some(std::ffi::OsStr::new("home"))
        || !base
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("among-rust-display-"))
    {
        return Err("drag source requires owned display evidence".into());
    }
    let display = std::env::var("DISPLAY")?;
    for marker in ["xvfb-display.txt", "xephyr-display.txt"] {
        let path = base.join(marker);
        if path.exists()
            && !path.symlink_metadata()?.file_type().is_symlink()
            && std::fs::read_to_string(path)? == display
        {
            return Ok(());
        }
    }
    Err("drag source DISPLAY does not match owned evidence".into())
}

fn provider(kind: &str, file: &str) -> Result<gdk::ContentProvider, Box<dyn std::error::Error>> {
    Ok(match kind {
        "text" => gdk::ContentProvider::for_value(&"4".to_value()),
        "file" => gdk::ContentProvider::for_value(&gio::File::for_path(file).to_value()),
        // Eight little-endian bytes of a pane id under a private format: what a
        // bare number from another program would look like on the wire.
        "u64" => gdk::ContentProvider::for_bytes(
            "application/x-among-foreign-u64",
            &glib::Bytes::from_owned(4u64.to_le_bytes()),
        ),
        _ => return Err("payload must be text, file or u64".into()),
    })
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let [runner_home, kind, file, ready] = args.as_slice() else {
        return Err(
            "usage: check-drag-source OWNED_DISPLAY_HOME text|file|u64 FILE READY_MARKER".into(),
        );
    };
    owned_display(runner_home)?;
    gtk::init()?;
    let content = provider(kind, file)?;
    let label = gtk::Label::new(Some(kind));
    label.set_hexpand(true);
    label.set_vexpand(true);
    let source = gtk::DragSource::new();
    source.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE);
    source.connect_prepare(move |_, _, _| Some(content.clone()));
    // Markers beside the ready file prove that a native drag really started and
    // finished; checks that only look for the absence of an effect would
    // otherwise pass when no drag happened at all.
    let began = PathBuf::from(format!("{ready}.began"));
    source.connect_drag_begin(move |_, _| {
        let _ = std::fs::write(&began, b"began");
    });
    let ended = PathBuf::from(format!("{ready}.ended"));
    let cancelled = ended.clone();
    source.connect_drag_end(move |_, _, _| {
        let _ = std::fs::write(&ended, b"ended");
    });
    source.connect_drag_cancel(move |_, _, _| {
        let _ = std::fs::write(&cancelled, b"cancelled");
        false
    });
    label.add_controller(source);
    let window = gtk::Window::builder()
        .title(TITLE)
        .default_width(240)
        .default_height(160)
        .child(&label)
        .build();
    let main_loop = glib::MainLoop::new(None, false);
    let ready = PathBuf::from(ready);
    window.connect_map(move |_| {
        // The marker tells the check the window can be found and dragged from.
        let _ = std::fs::write(&ready, b"mapped");
    });
    let finished = main_loop.clone();
    window.connect_close_request(move |_| {
        finished.quit();
        glib::Propagation::Proceed
    });
    window.present();
    main_loop.run();
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("drag source fixture: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
