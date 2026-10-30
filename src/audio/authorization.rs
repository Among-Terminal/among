//! per-operation shared start authority, independent of spawn observation.
use std::{
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::FileExt,
    },
    sync::atomic::{AtomicU32, Ordering},
};

const PENDING: u32 = 0;
const GRANTED: u32 = 1;
const REVOKED: u32 = 2;
const CLAIMED: u32 = 3;
const MAGIC: u32 = 0x414e4752;
const SEALS: i32 = libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;

#[repr(C)]
struct Control {
    id: u64,
    state: AtomicU32,
    magic: u32,
}

pub(super) struct Authorization {
    file: File,
    control: *mut Control,
}

// the fixed-size MAP_SHARED mapping outlives every reader through Arc ownership.
// headers are immutable after publication; only the lock-free atomic is mutated.
unsafe impl Send for Authorization {}
unsafe impl Sync for Authorization {}

impl Authorization {
    pub(super) fn new(id: u64) -> std::io::Result<Self> {
        Self::new_bound(id, &[])
    }

    pub(super) fn new_bound(id: u64, binding: &[u8]) -> std::io::Result<Self> {
        if binding.len() > crate::notifications::MAX_EFFECT_BINDING {
            return Err(std::io::Error::other(
                "audio effect binding exceeds its bound",
            ));
        }
        let fd = unsafe {
            libc::memfd_create(
                c"among-audio-authority".as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        // Command replaces 0/1/2 before pre_exec. Closed caller stdio must not
        // let those replacements overwrite the authority descriptor.
        if file.as_raw_fd() < 3 {
            let reserved = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
            if reserved < 0 {
                return Err(std::io::Error::last_os_error());
            }
            file = unsafe { File::from_raw_fd(reserved) };
        }
        let payload = if binding.is_empty() {
            0
        } else {
            crate::notifications::MAX_EFFECT_BINDING
        };
        file.set_len((std::mem::size_of::<Control>() + payload) as u64)?;
        if !binding.is_empty() {
            file.write_all_at(binding, std::mem::size_of::<Control>() as u64)?;
        }
        let authority = Self::map(file)?;
        unsafe {
            authority.control.write(Control {
                id,
                state: AtomicU32::new(PENDING),
                magic: MAGIC,
            });
        }
        if unsafe { libc::fcntl(authority.fd(), libc::F_ADD_SEALS, SEALS) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(authority)
    }

    pub(super) fn receive(file: File, id: u64) -> std::io::Result<Self> {
        Self::receive_bound(file, id, &[])
    }

    pub(super) fn receive_bound(file: File, id: u64, binding: &[u8]) -> std::io::Result<Self> {
        if binding.len() > crate::notifications::MAX_EFFECT_BINDING {
            return Err(std::io::Error::other("invalid audio effect binding length"));
        }
        let payload = if binding.is_empty() {
            0
        } else {
            crate::notifications::MAX_EFFECT_BINDING
        };
        let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
        if seals < 0
            || seals & SEALS != SEALS
            || file.metadata()?.len() != (std::mem::size_of::<Control>() + payload) as u64
        {
            return Err(std::io::Error::other("invalid audio start authority"));
        }
        if !binding.is_empty() {
            // the trusted owned-operation route supplies these expected bytes;
            // received scope FDs cannot choose their own expected binding or
            // downgrade a bound event to the legacy/explicit-preview protocol.
            let mut expected = [0_u8; crate::notifications::MAX_EFFECT_BINDING];
            file.read_exact_at(&mut expected, std::mem::size_of::<Control>() as u64)?;
            if &expected[..binding.len()] != binding
                || expected[binding.len()..].iter().any(|byte| *byte != 0)
            {
                return Err(std::io::Error::other(
                    "audio operation/effect binding mismatch",
                ));
            }
        }
        // the scanner and other native exec children must not inherit authority.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let authority = Self::map(file)?;
        if authority.control().id != id || authority.control().magic != MAGIC {
            return Err(std::io::Error::other(
                "audio start authority belongs to another operation",
            ));
        }
        Ok(authority)
    }

    fn map(file: File) -> std::io::Result<Self> {
        let control = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                std::mem::size_of::<Control>(),
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if control == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        if control.is_null() {
            unsafe {
                libc::munmap(control, std::mem::size_of::<Control>());
            }
            return Err(std::io::Error::other(
                "audio authority mapping has no address",
            ));
        }
        Ok(Self {
            file,
            control: control.cast(),
        })
    }
    fn control(&self) -> &Control {
        unsafe { &*self.control }
    }
    pub(super) fn fd(&self) -> i32 {
        self.file.as_raw_fd()
    }
    pub(super) fn id(&self) -> u64 {
        self.control().id
    }
    pub(super) fn revoke(&self) {
        self.control().state.store(REVOKED, Ordering::SeqCst);
    }
    pub(super) fn revoked(&self) -> bool {
        self.control().state.load(Ordering::SeqCst) == REVOKED
    }
    pub(super) fn grant(&self) -> bool {
        self.control()
            .state
            .compare_exchange(PENDING, GRANTED, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
    pub(super) fn claim(&self) -> Result<bool, &'static str> {
        match self.control().state.compare_exchange(
            GRANTED,
            CLAIMED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => Ok(true),
            Err(PENDING) => Ok(false),
            Err(REVOKED) => Err("Audio operation was cancelled before playback."),
            Err(_) => Err("Audio start authority is invalid or was already consumed."),
        }
    }
}

impl Drop for Authorization {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.control.cast(), std::mem::size_of::<Control>());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_is_single_use_and_cancellation_cannot_be_regranted() -> std::io::Result<()> {
        let pending = Authorization::new(7)?;
        assert_eq!(pending.claim(), Ok(false));
        pending.revoke();
        assert!(!pending.grant());
        assert!(pending.claim().is_err());
        let granted = Authorization::new(8)?;
        assert!(granted.grant());
        assert_eq!(granted.claim(), Ok(true));
        assert!(granted.claim().is_err());
        granted.revoke();
        assert!(!granted.grant());
        Ok(())
    }

    #[test]
    fn a_distinct_operation_or_wrong_identity_has_no_start_authority() -> std::io::Result<()> {
        let old = Authorization::new(9)?;
        let current = Authorization::new(10)?;
        let child = Authorization::receive(current.file.try_clone()?, 10)?;
        assert!(Authorization::receive(current.file.try_clone()?, 9).is_err());
        assert!(current.grant());
        old.revoke();
        assert_eq!(child.claim(), Ok(true));
        assert!(child.claim().is_err());
        assert!(current.file.set_len(0).is_err());
        assert!(current.file.set_len(32).is_err());
        Ok(())
    }

    #[test]
    fn a_bound_operation_cannot_substitute_or_omit_its_trusted_binding() -> std::io::Result<()> {
        let binding = [41_u8; 328];
        let operation = Authorization::new_bound(11, &binding)?;
        assert!(Authorization::receive(operation.file.try_clone()?, 11).is_err());
        assert!(Authorization::receive_bound(operation.file.try_clone()?, 12, &binding).is_err());
        assert!(Authorization::receive_bound(operation.file.try_clone()?, 11, &[42; 328]).is_err());
        let imported = Authorization::receive_bound(operation.file.try_clone()?, 11, &binding)?;
        assert!(operation.grant());
        assert_eq!(imported.claim(), Ok(true));
        assert!(imported.claim().is_err());
        assert!(!operation.grant());
        Ok(())
    }
}
