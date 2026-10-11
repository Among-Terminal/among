use std::{os::unix::process::CommandExt, path::Path, process::Command};

fn launch(root: &Path) -> std::io::Error {
    // linuxdeploy patches ELF RUNPATHs; leaving the loader environment untouched
    // prevents bundled GTK/libstdc++ from replacing libraries in host shells.
    Command::new(root.join("usr/bin/among"))
        .args(std::env::args_os().skip(1))
        .env("AMONG_APPDIR", root)
        .exec()
}

fn main() {
    let result = std::env::current_exe().and_then(|path| {
        let root = path
            .parent()
            .ok_or_else(|| std::io::Error::other("AppRun root absent"))?;
        Err::<(), _>(launch(root))
    });
    if let Err(error) = result {
        eprintln!("Among AppImage: {error}");
        std::process::exit(1);
    }
}
