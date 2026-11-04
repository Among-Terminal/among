//! external malformed-input interposer; never linked into the application.
use std::{
    ffi::CStr,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

static NATIVE: AtomicUsize = AtomicUsize::new(0);
static FAULT: AtomicU8 = AtomicU8::new(0);

pub fn initialize() -> Result<(), Box<dyn std::error::Error>> {
    let symbol = unsafe { libc::dlsym(libc::RTLD_NEXT, c"execvp".as_ptr()) };
    if symbol.is_null() {
        return Err("native execvp unavailable for external probe".into());
    }
    NATIVE.store(symbol as usize, Ordering::Release);
    Ok(())
}

pub fn configure(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mode = match name {
        "none" => 0,
        "binding-nonce" => 1,
        "binding-job" => 2,
        "operation" => 3,
        "missing-fd" => 4,
        "aliased-fd" => 5,
        "reordered-fds" => 6,
        _ => return Err("unknown external input fault".into()),
    };
    FAULT.store(mode, Ordering::Release);
    Ok(())
}

/// # Safety
/// forwards execvp's valid C string/vector ABI. The owned parent resolves the
/// native function before workers/fork; the child uses only atomics and stack
/// buffers, not allocation, locks, filesystem I/O or fabricated core authority.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn execvp(
    file: *const libc::c_char,
    argv: *const *const libc::c_char,
) -> libc::c_int {
    let symbol = NATIVE.load(Ordering::Acquire);
    if symbol == 0 {
        unsafe {
            *libc::__errno_location() = libc::ENOSYS;
        }
        return -1;
    }
    let native: unsafe extern "C" fn(
        *const libc::c_char,
        *const *const libc::c_char,
    ) -> libc::c_int = unsafe { std::mem::transmute(symbol) };
    let mode = FAULT.load(Ordering::Acquire);
    if mode == 0 || argv.is_null() {
        return unsafe { native(file, argv) };
    }
    let mut args = [std::ptr::null(); 10];
    let mut count = 0;
    while count < 9 {
        let value = unsafe { *argv.add(count) };
        if value.is_null() {
            break;
        }
        args[count] = value;
        count += 1;
    }
    if count != 8 || unsafe { CStr::from_ptr(args[1]) }.to_bytes() != b"--among-audio-decoder-v2" {
        return unsafe { native(file, argv) };
    }
    let mut buffer = [0_u8; 1025];
    let index = if mode <= 2 {
        6
    } else if mode == 3 {
        4
    } else {
        7
    };
    let original = unsafe { CStr::from_ptr(args[index]) }.to_bytes();
    if original.len() >= buffer.len() {
        return unsafe { native(file, argv) };
    }
    match mode {
        1 | 2 => {
            buffer[..original.len()].copy_from_slice(original);
            let offset = if mode == 1 { 80 * 2 } else { 24 * 2 };
            if original.len() <= offset {
                return unsafe { native(file, argv) };
            }
            buffer[offset] = if buffer[offset] == b'0' { b'1' } else { b'0' };
        }
        3 => {
            if original.is_empty() {
                return unsafe { native(file, argv) };
            }
            buffer[..original.len()].copy_from_slice(original);
            buffer[0] = if buffer[0] == b'0' { b'1' } else { b'0' };
        }
        4 => buffer[..10].copy_from_slice(b"2147483647"),
        5 | 6 => {
            let Some(first) = original.iter().position(|&byte| byte == b',') else {
                return unsafe { native(file, argv) };
            };
            if mode == 5 {
                if first * 2 + 1 >= buffer.len() {
                    return unsafe { native(file, argv) };
                }
                buffer[..first].copy_from_slice(&original[..first]);
                buffer[first] = b',';
                buffer[first + 1..first * 2 + 1].copy_from_slice(&original[..first]);
            } else {
                let next = original[first + 1..]
                    .iter()
                    .position(|&byte| byte == b',')
                    .map_or(original.len(), |end| first + 1 + end);
                let second = &original[first + 1..next];
                buffer[..second.len()].copy_from_slice(second);
                buffer[second.len()] = b',';
                buffer[second.len() + 1..second.len() + 1 + first]
                    .copy_from_slice(&original[..first]);
                buffer[next..original.len()].copy_from_slice(&original[next..]);
            }
        }
        _ => return unsafe { native(file, argv) },
    }
    args[index] = buffer.as_ptr().cast();
    unsafe { native(file, args.as_ptr()) }
}
