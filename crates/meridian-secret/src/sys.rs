//! The operating system calls a secret's memory needs. This is the only
//! module in the workspace that uses `unsafe`, and it is kept small enough
//! to read in one sitting.
//!
//! What it gives: a region of memory of a fixed size that
//!
//! - sits between two pages nothing can read or write, so running off either
//!   end faults instead of touching a neighbour;
//! - is locked, so it is never written to swap;
//! - is left out of core dumps, and reads as zeroes in a forked child;
//! - is overwritten with zeroes before it is given back.
//!
//! Every safety argument is the same one: the region is a private anonymous
//! mapping this module made, of a length it remembers, that nothing else has
//! a pointer to, and it is unmapped exactly once, when the owner is dropped.
#![allow(unsafe_code)]

use zeroize::Zeroize;

/// What the operating system agreed to do for a region.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Protection {
    /// The pages are locked into memory and cannot be swapped out.
    pub locked: bool,
    /// The pages are left out of a core dump.
    pub undumpable: bool,
    /// The pages sit between two inaccessible guard pages.
    pub guarded: bool,
}

#[cfg(unix)]
mod imp {
    use super::Protection;
    use std::ptr::NonNull;

    pub(crate) struct Region {
        /// The start of the whole mapping, guard pages included.
        base: NonNull<u8>,
        /// The length of the whole mapping.
        mapped: usize,
        /// The start of the bytes the owner may use.
        data: NonNull<u8>,
        len: usize,
        protection: Protection,
    }

    // SAFETY: the region owns its mapping outright and hands out references
    // only through `&self` and `&mut self`, so it follows the same rules as
    // a `Box<[u8]>`.
    unsafe impl Send for Region {}
    unsafe impl Sync for Region {}

    fn page_size() -> usize {
        // SAFETY: sysconf reads a constant of the running system.
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        usize::try_from(size)
            .ok()
            .filter(|s| *s > 0)
            .unwrap_or(4096)
    }

    impl Region {
        pub(crate) fn new(len: usize) -> std::io::Result<Region> {
            let page = page_size();
            let data_pages = len.max(1).div_ceil(page);
            let data_len = data_pages
                .checked_mul(page)
                .ok_or_else(|| std::io::Error::other("a secret of that size cannot be held"))?;
            let mapped = data_len
                .checked_add(2 * page)
                .ok_or_else(|| std::io::Error::other("a secret of that size cannot be held"))?;

            // SAFETY: a fresh private anonymous mapping; no existing memory
            // is touched. It starts with no access at all.
            let base = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    mapped,
                    libc::PROT_NONE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            if base == libc::MAP_FAILED {
                return Err(std::io::Error::last_os_error());
            }
            let base = base.cast::<u8>();
            // SAFETY: `page` is inside the mapping, which is `mapped` long.
            let usable = unsafe { base.add(page) };

            // SAFETY: the pages between the guards are inside the mapping.
            // The first and last page stay PROT_NONE: they are the guards.
            let opened = unsafe {
                libc::mprotect(usable.cast(), data_len, libc::PROT_READ | libc::PROT_WRITE)
            };
            if opened != 0 {
                let err = std::io::Error::last_os_error();
                // SAFETY: unmapping the mapping made above, once.
                unsafe { libc::munmap(base.cast(), mapped) };
                return Err(err);
            }

            // SAFETY: locking and advising pages this mapping owns. A
            // failure of any of these leaves the memory usable, so each is
            // recorded and not fatal here: the caller decides.
            let locked = unsafe { libc::mlock(usable.cast(), data_len) } == 0;
            #[cfg(target_os = "linux")]
            let undumpable =
                unsafe { libc::madvise(usable.cast(), data_len, libc::MADV_DONTDUMP) } == 0;
            #[cfg(not(target_os = "linux"))]
            let undumpable = false;
            #[cfg(target_os = "linux")]
            // SAFETY: as above. Older kernels do not know the flag, and the
            // answer is not needed: a child that forks gets a copy either
            // way, and this only makes that copy zeroes where it can.
            unsafe {
                libc::madvise(usable.cast(), data_len, libc::MADV_WIPEONFORK);
            }

            // The bytes are placed at the end of the usable pages, so the
            // byte after the last one is the guard page.
            // SAFETY: `data_len - len` is within the usable pages.
            let data = unsafe { usable.add(data_len - len) };
            let (Some(base), Some(data)) = (NonNull::new(base), NonNull::new(data)) else {
                return Err(std::io::Error::other("the mapping has no address"));
            };
            Ok(Region {
                base,
                mapped,
                data,
                len,
                protection: Protection {
                    locked,
                    undumpable,
                    guarded: true,
                },
            })
        }

        pub(crate) fn protection(&self) -> Protection {
            self.protection
        }

        pub(crate) fn bytes(&self) -> &[u8] {
            // SAFETY: `data` points at `len` readable bytes this region
            // owns, alive for as long as `self` is.
            unsafe { std::slice::from_raw_parts(self.data.as_ptr(), self.len) }
        }

        pub(crate) fn bytes_mut(&mut self) -> &mut [u8] {
            // SAFETY: as `bytes`, and `&mut self` makes it the only
            // reference.
            unsafe { std::slice::from_raw_parts_mut(self.data.as_ptr(), self.len) }
        }
    }

    impl Drop for Region {
        fn drop(&mut self) {
            use zeroize::Zeroize;
            self.bytes_mut().zeroize();
            // SAFETY: the mapping was made by `new`, is `mapped` long, and
            // is unmapped here and nowhere else. Unmapping also unlocks.
            unsafe {
                libc::munmap(self.base.as_ptr().cast(), self.mapped);
            }
        }
    }

    /// Turns off core dumps for this process and, on Linux, makes it
    /// undumpable, which also stops another process of the same user
    /// attaching a debugger or reading its memory through /proc.
    pub(crate) fn harden() -> (bool, bool) {
        let none = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: setrlimit reads the struct it is given and changes a
        // limit of this process.
        let no_core = unsafe { libc::setrlimit(libc::RLIMIT_CORE, &none) } == 0;
        #[cfg(target_os = "linux")]
        // SAFETY: prctl with PR_SET_DUMPABLE takes an integer and changes a
        // flag of this process.
        let undumpable = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } == 0;
        #[cfg(not(target_os = "linux"))]
        let undumpable = false;
        (no_core, undumpable)
    }

    /// Whether a core dump of this process could be written, and whether it
    /// is dumpable, as the system reports them now.
    pub(crate) fn hardened() -> (bool, bool) {
        let mut limit = libc::rlimit {
            rlim_cur: 1,
            rlim_max: 1,
        };
        // SAFETY: getrlimit writes the struct it is given.
        let read = unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut limit) } == 0;
        let no_core = read && limit.rlim_cur == 0;
        #[cfg(target_os = "linux")]
        // SAFETY: prctl with PR_GET_DUMPABLE reads a flag of this process.
        let undumpable = unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) } == 0;
        #[cfg(not(target_os = "linux"))]
        let undumpable = false;
        (no_core, undumpable)
    }

    /// The numeric id of the user this process runs as.
    pub(crate) fn user_id() -> u32 {
        // SAFETY: getuid has no arguments and cannot fail.
        unsafe { libc::getuid() }
    }

    /// Whether a process with this id exists.
    pub(crate) fn process_exists(pid: u32) -> bool {
        // 0 is not a process: to `kill` it means this process's whole
        // group, which always exists.
        let Ok(pid @ 1..) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: signal 0 sends nothing; it only asks whether the process
        // could be signalled.
        let answer = unsafe { libc::kill(pid, 0) };
        answer == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    /// The kernel keyring's user keyring and session keyring, as the
    /// special ids `keyctl(2)` takes.
    #[cfg(target_os = "linux")]
    const KEYRINGS: [libc::c_long; 2] = [-3, -4];
    #[cfg(target_os = "linux")]
    const USER_KEYRING: libc::c_long = -4;
    #[cfg(target_os = "linux")]
    const KEYCTL_SEARCH: libc::c_long = 10;
    #[cfg(target_os = "linux")]
    const KEYCTL_READ: libc::c_long = 11;
    #[cfg(target_os = "linux")]
    const THREAD_KEYRING: libc::c_long = -1;
    #[cfg(target_os = "linux")]
    const KEYCTL_SETPERM: libc::c_long = 5;
    #[cfg(target_os = "linux")]
    const KEYCTL_LINK: libc::c_long = 8;
    #[cfg(target_os = "linux")]
    const KEYCTL_UNLINK: libc::c_long = 9;
    /// The permissions a stored key is given: everything to a possessor,
    /// and view, read, write, search and setattr to the key's owner. A key
    /// made with `add_key` alone is readable only by a possessor, and a
    /// session that does not link the user keyring possesses nothing in it,
    /// so a value stored by one command would be unreadable by the next.
    #[cfg(target_os = "linux")]
    const STORED_KEY_PERM: libc::c_long = 0x3f3f_0000;
    #[cfg(target_os = "linux")]
    const KEY_TYPE: &std::ffi::CStr = c"user";

    /// Finds a key of type `user` by its description, in the session
    /// keyring and then the user keyring. `None` when neither holds it.
    #[cfg(target_os = "linux")]
    pub(crate) fn keyring_find(description: &std::ffi::CStr) -> std::io::Result<Option<i64>> {
        for keyring in KEYRINGS {
            // SAFETY: both strings are NUL-terminated and outlive the call;
            // the call reads them and returns an id or an error.
            let id = unsafe {
                libc::syscall(
                    libc::SYS_keyctl,
                    KEYCTL_SEARCH,
                    keyring,
                    KEY_TYPE.as_ptr(),
                    description.as_ptr(),
                    0 as libc::c_long,
                )
            };
            if id > 0 {
                return Ok(Some(id));
            }
            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
                // Not in this keyring, or the keyring itself is not there.
                Some(libc::ENOKEY | libc::EKEYEXPIRED | libc::EKEYREVOKED) => {}
                _ => return Err(error),
            }
        }
        Ok(None)
    }

    /// How many bytes the key holds.
    #[cfg(target_os = "linux")]
    pub(crate) fn keyring_len(id: i64) -> std::io::Result<usize> {
        // SAFETY: a null buffer of no length asks only for the size.
        let len = unsafe {
            libc::syscall(
                libc::SYS_keyctl,
                KEYCTL_READ,
                id as libc::c_long,
                std::ptr::null_mut::<u8>(),
                0 as libc::c_long,
            )
        };
        usize::try_from(len).map_err(|_| std::io::Error::last_os_error())
    }

    /// Reads the key into `into`, and says how many bytes the key holds.
    #[cfg(target_os = "linux")]
    pub(crate) fn keyring_read(id: i64, into: &mut [u8]) -> std::io::Result<usize> {
        // SAFETY: the buffer is valid for `into.len()` bytes for the whole
        // call, and the kernel writes no more than that.
        let len = unsafe {
            libc::syscall(
                libc::SYS_keyctl,
                KEYCTL_READ,
                id as libc::c_long,
                into.as_mut_ptr(),
                into.len() as libc::c_long,
            )
        };
        usize::try_from(len).map_err(|_| std::io::Error::last_os_error())
    }

    /// Puts a key of type `user` in the user keyring, replacing one of the
    /// same description.
    ///
    /// The key is made in this thread's own keyring first, so that this
    /// process possesses it and may set its permissions, and is then linked
    /// into the user keyring, where a link displaces any key of the same
    /// description. Made in the user keyring directly, a process whose
    /// session does not link that keyring would own the key and still not
    /// be allowed to change it or read it back.
    #[cfg(target_os = "linux")]
    pub(crate) fn keyring_store(description: &std::ffi::CStr, value: &[u8]) -> std::io::Result<()> {
        // SAFETY: the strings are NUL-terminated, the payload is valid for
        // `value.len()` bytes, and the kernel copies it before returning.
        let id = unsafe {
            libc::syscall(
                libc::SYS_add_key,
                KEY_TYPE.as_ptr(),
                description.as_ptr(),
                value.as_ptr(),
                value.len(),
                THREAD_KEYRING,
            )
        };
        if id <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: each call takes the id the kernel just returned and a
        // keyring's special id, and changes nothing in this process's memory.
        let set = unsafe { libc::syscall(libc::SYS_keyctl, KEYCTL_SETPERM, id, STORED_KEY_PERM) };
        if set < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let linked = unsafe { libc::syscall(libc::SYS_keyctl, KEYCTL_LINK, id, USER_KEYRING) };
        if linked < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // The thread keyring dies with the thread; the key is unlinked from
        // it now so that it is in one place. Failing to is no failure.
        unsafe { libc::syscall(libc::SYS_keyctl, KEYCTL_UNLINK, id, THREAD_KEYRING) };
        Ok(())
    }

    /// Reads from standard input straight into `into`, with no buffer of
    /// the standard library's in between to hold a copy.
    pub(crate) fn stdin_read(into: &mut [u8]) -> std::io::Result<usize> {
        // SAFETY: the buffer is valid for `into.len()` bytes for the call.
        let read = unsafe { libc::read(0, into.as_mut_ptr().cast(), into.len()) };
        usize::try_from(read).map_err(|_| std::io::Error::last_os_error())
    }

    /// What a terminal was like before its echo was turned off.
    pub(crate) type Echo = libc::termios;

    /// Stops what is typed at the terminal on `fd` from being shown, and
    /// gives back what to restore. `None` when `fd` is not a terminal.
    pub(crate) fn echo_off(fd: i32) -> Option<Echo> {
        // SAFETY: termios is plain data, and tcgetattr fills it or fails.
        let mut was: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `was` is valid for the call.
        if unsafe { libc::tcgetattr(fd, &mut was) } != 0 {
            return None;
        }
        let mut quiet = was;
        quiet.c_lflag &= !libc::ECHO;
        quiet.c_lflag |= libc::ECHONL;
        // SAFETY: `quiet` is a valid termios read from this terminal.
        (unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &quiet) } == 0).then_some(was)
    }

    /// Puts a terminal back as [`echo_off`] found it.
    pub(crate) fn echo_restore(fd: i32, was: &Echo) {
        // SAFETY: `was` came from tcgetattr on this terminal.
        unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, was) };
    }
}

#[cfg(not(unix))]
mod imp {
    use super::Protection;

    /// Where the system calls above are not available the bytes are still
    /// wiped when dropped and never reallocated; they are not locked or
    /// guarded, and `Protection` says so.
    pub(crate) struct Region {
        bytes: Box<[u8]>,
    }

    impl Region {
        pub(crate) fn new(len: usize) -> std::io::Result<Region> {
            Ok(Region {
                bytes: vec![0u8; len].into_boxed_slice(),
            })
        }

        pub(crate) fn protection(&self) -> Protection {
            Protection::default()
        }

        pub(crate) fn bytes(&self) -> &[u8] {
            &self.bytes
        }

        pub(crate) fn bytes_mut(&mut self) -> &mut [u8] {
            &mut self.bytes
        }
    }

    impl Drop for Region {
        fn drop(&mut self) {
            use zeroize::Zeroize;
            self.bytes.zeroize();
        }
    }

    pub(crate) fn harden() -> (bool, bool) {
        (false, false)
    }

    pub(crate) fn hardened() -> (bool, bool) {
        (false, false)
    }

    pub(crate) fn user_id() -> u32 {
        0
    }

    pub(crate) fn process_exists(_pid: u32) -> bool {
        true
    }

    /// Reads from standard input. Not yet unbuffered on this platform.
    pub(crate) fn stdin_read(into: &mut [u8]) -> std::io::Result<usize> {
        use std::io::Read;
        std::io::stdin().read(into)
    }

    /// Echo is not turned off on this platform yet.
    pub(crate) type Echo = ();

    pub(crate) fn echo_off(_fd: i32) -> Option<Echo> {
        None
    }

    pub(crate) fn echo_restore(_fd: i32, _was: &Echo) {}
}

pub(crate) use imp::{
    echo_off, echo_restore, harden, hardened, process_exists, stdin_read, user_id, Region,
};
#[cfg(target_os = "linux")]
pub(crate) use imp::{keyring_find, keyring_len, keyring_read, keyring_store};

/// Overwrites `bytes` with zeroes in a way the compiler cannot remove.
pub(crate) fn wipe(bytes: &mut [u8]) {
    bytes.zeroize();
}
