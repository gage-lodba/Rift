//! Native single-instance guard, replacing the old desktop shell's plugin.
//!
//! The old app used that plugin to make a second launch focus the running
//! window instead of starting a rival instance (two audio threads fighting over
//! the output device, two copies of the playback snapshot, etc.). With no IPC
//! channel to ask the first instance to come forward, the native contract is
//! simpler and documented: **a second launch exits immediately**; it does not
//! focus the existing window.
//!
//! # How it works
//!
//! * **Windows**: a named mutex in the local session namespace
//!   (`Local\dev.jerimiah.rift-single-instance`) via `CreateMutexW` /
//!   `CloseHandle` from `kernel32` — raw FFI, no extra dependency. The first
//!   process creates and keeps the handle; a second process's `CreateMutexW`
//!   succeeds but reports `ERROR_ALREADY_EXISTS`, and it exits.
//! * **Linux/macOS/BSD**: an advisory `flock(LOCK_EX | LOCK_NB)` on
//!   `<data dir>/.single-instance.lock` (raw `libc` FFI, no dependency). The
//!   kernel releases the lock when the process exits — including a crash — so
//!   no stale lock file can wedge the next launch.
//! * **Other platforms**: the guard always succeeds (no-op); documented
//!   degradation rather than a startup failure on an untested target.
//!
//! # Integration for `main.rs` (the Lead owns that file)
//!
//! Acquire **first**, before creating the window, the backend or any threads,
//! and keep the guard alive for the whole session:
//!
//! ```text
//! fn main() -> anyhow::Result<()> {
//!     let _instance = match single_instance::acquire() {
//!         Some(instance) => instance,
//!         None => {
//!             // Another Rift owns the audio device and library files.
//!             eprintln!("Rift is already running.");
//!             return Ok(());
//!         }
//!     };
//!     // ... run the app ...
//! }
//! ```
//!
//! Note the ordering constraint: acquiring after the window/backend exists is
//! still correct, but a second launch would then pay startup cost (window,
//! audio device, runtime threads) only to exit, and would briefly fight the
//! first instance — so acquire before anything expensive.

#[cfg(unix)]
use crate::window_state::default_data_dir;
use crate::window_state::APP_IDENTIFIER;

/// Stable resource name for the guard (suffix last: the same identifier the data
/// directory uses).
pub fn instance_name() -> String {
    #[cfg(windows)]
    {
        // `Local\` = per-session object namespace: two users on one machine each
        // get their own instance, which is what a per-user desktop app wants.
        format!("Local\\{APP_IDENTIFIER}-single-instance")
    }
    #[cfg(not(windows))]
    {
        format!("{APP_IDENTIFIER}-single-instance")
    }
}

/// The lock file used on Unix-like platforms.
#[cfg(unix)]
fn lock_path() -> Option<std::path::PathBuf> {
    default_data_dir()
        .or_else(|| Some(std::env::temp_dir()))
        .map(|dir| dir.join(".single-instance.lock"))
}

/// RAII holder of the single-instance lock.
///
/// Acquire it once at the start of the process and keep it alive; dropping it
/// (or the process exiting) releases the lock.
pub struct SingleInstance {
    _guard: platform::Guard,
}

impl SingleInstance {
    /// Try to become the one running instance.
    ///
    /// `None` means another instance already holds the lock (or the platform
    /// resource could not be created — treated conservatively as "busy"; the
    /// caller should exit rather than risk two writers).
    pub fn acquire() -> Option<Self> {
        acquire_platform().map(|guard| Self { _guard: guard })
    }
}

/// Convenience wrapper for [`SingleInstance::acquire`].
pub fn acquire() -> Option<SingleInstance> {
    SingleInstance::acquire()
}

/// Diagnostic probe: is another process holding the guard right now?
///
/// This briefly acquires and releases the lock when nothing else holds it, so it
/// is only suitable for logging/diagnostics — the startup path must call
/// [`acquire`] and hold the result.
pub fn is_already_running() -> bool {
    match SingleInstance::acquire() {
        Some(instance) => {
            drop(instance);
            false
        }
        None => true,
    }
}

fn acquire_platform() -> Option<platform::Guard> {
    #[cfg(windows)]
    {
        platform::Guard::acquire(&instance_name())
    }
    #[cfg(unix)]
    {
        platform::Guard::acquire(&lock_path()?)
    }
    #[cfg(not(any(windows, unix)))]
    {
        Some(platform::Guard)
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::ptr;

    /// `CreateMutexW` succeeded but the named object already existed: another
    /// Rift owns it.
    const ERROR_ALREADY_EXISTS: u32 = 183;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(
            mutex_attributes: *const c_void,
            initial_owner: i32,
            name: *const u16,
        ) -> *mut c_void;
        fn CloseHandle(object: *mut c_void) -> i32;
        fn GetLastError() -> u32;
        fn SetLastError(error: u32);
    }

    /// Owns the named mutex handle; closing it on drop releases the name.
    pub struct Guard(*mut c_void);

    impl Guard {
        /// Create/open the named mutex.
        ///
        /// Returns `None` when another process already owns it, and also when
        /// the OS refuses to create the object (conservative: don't start).
        pub fn acquire(name: &str) -> Option<Self> {
            let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            // `GetLastError` after `CreateMutexW` must describe this call, not a
            // previous one; reset it first so a stale value can't fake
            // `ERROR_ALREADY_EXISTS`.
            // SAFETY: no arguments; sets the calling thread's last-error value.
            unsafe { SetLastError(0) };
            // SAFETY: `wide` is a live, NUL-terminated UTF-16 buffer that outlives
            // the call, and a null security-attribute pointer is explicitly
            // allowed by `CreateMutexW`. The returned handle is owned by `Guard`
            // (or closed below) exactly once.
            let handle = unsafe { CreateMutexW(ptr::null(), 0, wide.as_ptr()) };
            if handle.is_null() {
                return None;
            }
            // SAFETY: no arguments; reads the last-error value set by the
            // `CreateMutexW` call above.
            let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            if already_exists {
                // The handle we were given is a reference to the existing
                // object; release our reference and report "busy".
                // SAFETY: `handle` is non-null (checked above) and came from
                // `CreateMutexW`, so this is its matching release; it is not used
                // afterwards.
                unsafe { CloseHandle(handle) };
                return None;
            }
            Some(Self(handle))
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` is a non-null handle created by `CreateMutexW`
                // in `acquire`; `Guard` is its unique owner, and the field is
                // nulled afterwards so a second drop cannot double-close it.
                unsafe { CloseHandle(self.0) };
                self.0 = ptr::null_mut();
            }
        }
    }
}

#[cfg(unix)]
mod platform {
    use std::fs::{File, OpenOptions};
    use std::os::unix::io::AsRawFd;
    use std::path::Path;

    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;

    extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }

    /// Holds the open lock file; dropping it closes the descriptor, which drops
    /// the flock, so no explicit unlock is needed (and a crash releases it too).
    pub struct Guard {
        _file: File,
    }

    impl Guard {
        /// Open/create `lock_path` and take a non-blocking exclusive lock.
        ///
        /// `None` means another process holds it (or the lock file cannot be
        /// created, which is treated the same conservatively).
        pub fn acquire(lock_path: &Path) -> Option<Self> {
            if let Some(parent) = lock_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .open(lock_path)
                .ok()?;
            // SAFETY: `file` owns a valid, open file descriptor for the duration
            // of the call; `flock` only reads that descriptor and never retains
            // it. A failure is reported as "not locked" (conservative).
            let locked = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0;
            if locked {
                Some(Self { _file: file })
            } else {
                None
            }
        }
    }
}

#[cfg(not(any(windows, unix)))]
mod platform {
    /// No-op guard: the platform has no implementation, so a single instance is
    /// not enforced (documented degradation rather than a startup failure).
    pub struct Guard;

    impl Guard {
        pub fn acquire(_resource: &str) -> Option<Self> {
            Some(Self)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique() -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    /// The resource name is derived from the same identifier as the data
    /// directory, so two installations of the same app (not two apps) collide.
    #[test]
    fn the_instance_name_names_the_app_and_is_stable() {
        let name = instance_name();
        assert!(name.contains(APP_IDENTIFIER), "unexpected name: {name}");
        assert_eq!(name, instance_name());
        #[cfg(windows)]
        assert!(
            name.starts_with("Local\\"),
            "the mutex must be per-session: {name}"
        );
    }

    /// Windows: the named mutex is exclusive while a guard is alive and
    /// re-acquirable once it is dropped. Uses a unique per-test name so a real
    /// running Rift (or another test) can't interfere.
    #[cfg(windows)]
    #[test]
    fn the_named_mutex_admits_one_holder_and_releases_on_drop() {
        let name = format!(
            "Local\\{APP_IDENTIFIER}-test-{}-{}",
            std::process::id(),
            unique()
        );
        let first = platform::Guard::acquire(&name).expect("the first holder wins");
        assert!(
            platform::Guard::acquire(&name).is_none(),
            "a second holder must be refused"
        );
        drop(first);
        assert!(
            platform::Guard::acquire(&name).is_some(),
            "dropping the guard must release the name"
        );
    }

    /// Unix: flock is exclusive while the file is held and released by closing
    /// it (the kernel also releases it on process death, so no stale lock).
    #[cfg(unix)]
    #[test]
    fn the_lock_file_admits_one_holder_and_releases_on_drop() {
        let dir = std::env::temp_dir().join(format!(
            "rift-single-instance-{}-{}",
            std::process::id(),
            unique()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join(".single-instance.lock");

        let first = platform::Guard::acquire(&path).expect("the first holder wins");
        assert!(
            platform::Guard::acquire(&path).is_none(),
            "a second holder must be refused"
        );
        drop(first);
        assert!(
            platform::Guard::acquire(&path).is_some(),
            "dropping the guard must release the lock"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The module-level API must compile and agree with the platform guard: two
    /// acquisitions in one process cannot both be the single instance.
    ///
    /// Skips when a real Rift instance is already running on the machine (that
    /// is the correct `None`, and the test cannot distinguish it from a bug).
    #[test]
    fn the_public_guard_is_exclusive_within_a_process() {
        let first = match SingleInstance::acquire() {
            Some(instance) => instance,
            None => return, // another Rift really is running; nothing to assert
        };
        assert!(is_already_running(), "a held guard must read as running");
        drop(first);
        assert!(
            SingleInstance::acquire().is_some(),
            "the guard must be re-acquirable after release"
        );
    }
}
