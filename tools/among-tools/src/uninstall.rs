fn main() -> std::process::ExitCode {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if matches!(args.as_slice(), [help] if help == "--help" || help == "-h") {
        println!("among-uninstall — remove only verified installer-owned files; preserve settings, sessions and plugins\nUsage: among-uninstall [--uninstall]");
        return std::process::ExitCode::SUCCESS;
    }
    let result = if args.is_empty() || args == ["--uninstall"] {
        among_tools::installation::Layout::discover().and_then(among_tools::installation::uninstall)
    } else {
        Err("invalid uninstaller arguments; use --help".into())
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Uninstallation failed: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
