use std::fs::{self, OpenOptions};
use std::io::{self, Write};

fn run() -> io::Result<i32> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() != 7
        || args[..6]
            != [
                "metadata",
                "--locked",
                "--offline",
                "--format-version",
                "1",
                "--manifest-path",
            ]
    {
        return Ok(90);
    }
    let executable = std::env::current_exe()?;
    let root = executable
        .parent()
        .ok_or_else(|| io::Error::other("fixture root absent"))?;
    writeln!(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("calls"))?,
        "{}",
        args[6]
    )?;
    let failure = fs::read_to_string(root.join("failure-manifest"))?;
    if args[6] == failure {
        io::stderr().write_all(&fs::read(root.join("failure-stderr"))?)?;
        return Ok(match failure.as_str() {
            "Cargo.toml" => 37,
            "examples/plugins/Cargo.toml" => 38,
            "tools/among-tools/Cargo.toml" => 39,
            _ => 91,
        });
    }
    io::stdout().write_all(&fs::read(root.join("graph.json"))?)?;
    Ok(0)
}

fn main() {
    std::process::exit(match run() {
        Ok(status) => status,
        Err(_) => 92,
    });
}
