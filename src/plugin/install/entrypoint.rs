//! the kernel must select the runtime we admitted, not a different interpreter
//! hidden behind a matching package hash. parsing never starts the executable.

use super::metadata::{Architecture, Artifact, Libc, Runtime};
use crate::plugin::data::Problem;
use std::fs::File;
use std::os::unix::fs::{FileExt, PermissionsExt};

const MAX_SHEBANG: usize = 256;
const MAX_PROGRAM_HEADERS: u16 = 128;

pub(super) fn validate(
    file: &File,
    artifact: &Artifact,
    check: impl Fn() -> Result<(), Problem>,
) -> Result<(), Problem> {
    check()?;
    match &artifact.runtime {
        Runtime::Script { executable, .. } => script(file, executable),
        Runtime::Native { libc, .. } => elf(file, &artifact.architecture, libc, &check),
    }
}

fn script(file: &File, declared: &str) -> Result<(), Problem> {
    let mut prefix = [0u8; MAX_SHEBANG];
    let read = file
        .read_at(&mut prefix, 0)
        .map_err(|_| Problem("cannot read bounded script entrypoint header"))?;
    let line = prefix[..read]
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or(Problem("script shebang must end with LF within 256 bytes"))?;
    if declared == "/usr/bin/env"
        || line < 2
        || !prefix[..read].starts_with(b"#!")
        || &prefix[2..line] != declared.as_bytes()
    {
        return Err(Problem("script shebang must name exactly the declared absolute interpreter; env, arguments and CRLF are unsupported"));
    }
    if !std::fs::metadata(declared)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    {
        return Err(Problem("declared script interpreter became unavailable"));
    }
    Ok(())
}

fn elf(
    file: &File,
    architecture: &Architecture,
    libc: &Libc,
    check: &impl Fn() -> Result<(), Problem>,
) -> Result<(), Problem> {
    let length = file
        .metadata()
        .map_err(|_| Problem("cannot inspect native entrypoint size"))?
        .len();
    let mut header = [0u8; 64];
    file.read_exact_at(&mut header, 0)
        .map_err(|_| Problem("native entrypoint is not a complete ELF64 header"))?;
    let machine = match architecture {
        Architecture::Amd64 => 62,
        Architecture::Arm64 => 183,
    };
    if &header[..4] != b"\x7fELF"
        || header[4] != 2
        || header[5] != 1
        || header[6] != 1
        || !matches!(header[7], 0 | 3)
        || header[8] != 0
        || !matches!(u16_at(&header, 16), 2 | 3)
        || u16_at(&header, 18) != machine
        || u32_at(&header, 20) != 1
        || u16_at(&header, 52) != 64
        || u16_at(&header, 54) != 56
        || u32_at(&header, 48) != 0
    {
        return Err(Problem("native entrypoint ELF class, byte order, machine or format disagrees with the selected Linux runtime"));
    }
    let phoff = u64_at(&header, 32);
    let count = u16_at(&header, 56);
    let entry = u64_at(&header, 24);
    if phoff < 64
        || count == 0
        || count > MAX_PROGRAM_HEADERS
        || phoff
            .checked_add(u64::from(count) * 56)
            .is_none_or(|end| end > length)
        || entry == 0
    {
        return Err(Problem(
            "native ELF program headers or entry address are invalid or exceed their budget",
        ));
    }
    let shoff = u64_at(&header, 40);
    let sections = u16_at(&header, 60);
    if sections > 0
        && (u16_at(&header, 58) != 64
            || shoff < 64
            || shoff
                .checked_add(u64::from(sections) * 64)
                .is_none_or(|end| end > length)
            || u16_at(&header, 62) >= sections)
    {
        return Err(Problem(
            "native ELF section table is outside its declared file",
        ));
    }
    let mut executable_entry = false;
    let mut interpreter = false;
    for index in 0..count {
        check()?;
        let mut program = [0u8; 56];
        file.read_exact_at(&mut program, phoff + u64::from(index) * 56)
            .map_err(|_| Problem("native ELF program header is truncated"))?;
        let kind = u32_at(&program, 0);
        let flags = u32_at(&program, 4);
        let offset = u64_at(&program, 8);
        let address = u64_at(&program, 16);
        let bytes = u64_at(&program, 32);
        let memory = u64_at(&program, 40);
        let alignment = u64_at(&program, 48);
        if matches!(kind, 1 | 3) && offset.checked_add(bytes).is_none_or(|end| end > length) {
            return Err(Problem("native ELF loadable resource is outside its file"));
        }
        if kind == 1 {
            let end = address
                .checked_add(memory)
                .ok_or(Problem("native ELF memory range overflow"))?;
            if memory < bytes
                || (alignment > 1
                    && (!alignment.is_power_of_two() || offset % alignment != address % alignment))
            {
                return Err(Problem("native ELF load segment is malformed"));
            }
            executable_entry |= flags & 1 != 0 && address <= entry && entry < end;
        } else if kind == 3 {
            if interpreter || !(2..=MAX_SHEBANG as u64).contains(&bytes) {
                return Err(Problem(
                    "native ELF interpreter declaration is duplicate or exceeds its budget",
                ));
            }
            interpreter = true;
            let mut path = [0u8; MAX_SHEBANG];
            let size = bytes as usize;
            file.read_exact_at(&mut path[..size], offset)
                .map_err(|_| Problem("native ELF interpreter path is truncated"))?;
            if path[size - 1] != 0 || path[..size - 1].contains(&0) {
                return Err(Problem("native ELF interpreter path is malformed"));
            }
            let path = std::str::from_utf8(&path[..size - 1])
                .map_err(|_| Problem("native ELF interpreter path is not supported text"))?;
            let accepted = match (architecture, libc) {
                (Architecture::Amd64, Libc::Glibc) => matches!(
                    path,
                    "/lib64/ld-linux-x86-64.so.2" | "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2"
                ),
                (Architecture::Arm64, Libc::Glibc) => matches!(
                    path,
                    "/lib/ld-linux-aarch64.so.1" | "/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1"
                ),
                (Architecture::Amd64, Libc::Musl) => path == "/lib/ld-musl-x86_64.so.1",
                (Architecture::Arm64, Libc::Musl) => path == "/lib/ld-musl-aarch64.so.1",
            };
            if !accepted
                || !std::fs::metadata(path).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            {
                return Err(Problem("native ELF interpreter does not match the declared libc/architecture or is unavailable"));
            }
        }
    }
    if !executable_entry {
        return Err(Problem(
            "native ELF entry address is not in an executable load segment",
        ));
    }
    Ok(())
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}
fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}
