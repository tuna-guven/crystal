//! Process hardening and sandbox: rlimits, prctl, mlockall, env allowlist,
//! tty checks, Landlock and seccomp, applied in the order docs/design/startup.md fixes.

use std::fmt;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::panic::{self, Location};

use rustix::fs::{CWD, Dir, Mode, OFlags, openat};
use rustix::io::Errno;
use rustix::mm::{MlockAllFlags, mlockall};
use rustix::process::{
    DumpableBehavior, Resource, Rlimit, getrlimit, set_dumpable_behavior, setrlimit, umask,
};
use rustix::thread::set_no_new_privs;

/// Smallest RLIMIT_MEMLOCK Crystal starts with. Below this, a large vault
/// could exhaust locked memory mid-session, and a failed allocation aborts
/// without zeroizing or saving. See docs/guide/setup.md.
pub const MEMLOCK_FLOOR: u64 = 256 * 1024 * 1024;

/// A hardening step that failed. Holds only fixed step names and numbers,
/// never any data.
#[derive(Debug)]
pub enum HardenError {
    Syscall {
        step: &'static str,
        errno: Errno,
    },
    MemlockTooLow {
        hard_limit: u64,
    },
    /// Lowest inherited fd number and how many there are. Fd numbers are
    /// not secret.
    InheritedFds {
        lowest: i32,
        count: usize,
    },
}

impl fmt::Display for HardenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syscall { step, errno } => write!(f, "{step} failed: {errno}"),
            Self::MemlockTooLow { hard_limit } => write!(
                f,
                "RLIMIT_MEMLOCK is {} KiB, need at least {} KiB to keep notes \
                 out of swap; see docs/guide/setup.md",
                hard_limit / 1024,
                MEMLOCK_FLOOR / 1024
            ),
            Self::InheritedFds { lowest, count } => write!(
                f,
                "{count} open file descriptor(s) inherited from the program that \
                 started Crystal, lowest {lowest}; they would stay usable inside \
                 the sandbox. Start Crystal from a plain shell, or close them \
                 (for example: crystal NAME {lowest}>&-)"
            ),
        }
    }
}

impl std::error::Error for HardenError {}

fn step_failed(step: &'static str) -> impl FnOnce(Errno) -> HardenError {
    move |errno| HardenError::Syscall { step, errno }
}

/// Startup step 1: replace the default panic hook. The default hook prints
/// the panic message, which could contain note text; this one prints only
/// where the panic happened.
#[expect(clippy::disallowed_methods, reason = "the panic report goes to stderr")]
pub fn install_panic_hook() {
    panic::set_hook(Box::new(|info| {
        // The hook runs before unwinding drops the terminal guard; restore
        // first, or the guard would clear this message off the screen.
        crate::term::restore_terminal();
        // Ignore write errors: panicking inside the hook would abort and
        // skip the Drop impls that zeroize memory.
        let _ = write_panic_report(&mut io::stderr(), info.location());
    }));
}

/// Startup step 2: no core dumps, no ptrace or /proc/pid/mem access by
/// same-user processes, no privilege gain through exec, and new files
/// readable by the owner only. Fails closed on the first error.
pub fn lock_down_process() -> Result<(), HardenError> {
    // Hard limit too, so nothing in the process can raise it again.
    let no_core = Rlimit {
        current: Some(0),
        maximum: Some(0),
    };
    setrlimit(Resource::Core, no_core).map_err(step_failed("setrlimit(RLIMIT_CORE)"))?;
    set_dumpable_behavior(DumpableBehavior::NotDumpable)
        .map_err(step_failed("prctl(PR_SET_DUMPABLE)"))?;
    set_no_new_privs(true).map_err(step_failed("prctl(PR_SET_NO_NEW_PRIVS)"))?;
    // umask cannot fail; it returns the previous mask, which we don't need.
    umask(Mode::RWXG | Mode::RWXO);
    Ok(())
}

/// Startup step 2, last part: refuse to run with any file descriptor above
/// stdin, stdout and stderr already open. Landlock does not restrict fds
/// opened before it, and seccomp allows read and write, so an inherited
/// socket or directory fd would bypass the sandbox; the age plugin would
/// inherit it too. Closing an fd Crystal does not own needs `unsafe`
/// (rustix has no safe close_range), so Crystal fails closed instead.
/// Must run before Crystal opens any fd of its own.
pub fn refuse_inherited_fds() -> Result<(), HardenError> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let fd = openat(CWD, "/proc/self/fd", flags, Mode::empty())
        .map_err(step_failed("open(/proc/self/fd)"))?;
    let own = fd.as_raw_fd();
    let dir = Dir::new(fd).map_err(step_failed("read(/proc/self/fd)"))?;
    let mut names = Vec::new();
    for entry in dir {
        let entry = entry.map_err(step_failed("read(/proc/self/fd)"))?;
        names.push(entry.file_name().to_bytes().to_vec());
    }
    let stray = stray_fds(names.iter().map(Vec::as_slice), own);
    match stray.iter().min() {
        None => Ok(()),
        Some(&lowest) => Err(HardenError::InheritedFds {
            lowest,
            count: stray.len(),
        }),
    }
}

/// Fd numbers from a /proc/self/fd listing that are neither stdio nor
/// `own`, the fd used to read the listing. `.` and `..` are skipped.
fn stray_fds<'a>(names: impl Iterator<Item = &'a [u8]>, own: i32) -> Vec<i32> {
    names
        .filter_map(|name| std::str::from_utf8(name).ok()?.parse::<i32>().ok())
        .filter(|&fd| fd > 2 && fd != own)
        .collect()
}

/// Environment variables Crystal may read (startup step 3). Add a variant
/// only when code needs it; reading anything else is a clippy error
/// (clippy.toml, disallowed-methods).
#[derive(Clone, Copy)]
pub enum EnvVar {
    /// Locates the state directory, ~/.local/state/crystal.
    Home,
}

/// The only place Crystal reads its environment. The environment itself is
/// never modified: `remove_var` is unsafe in edition 2024.
#[expect(clippy::disallowed_methods, reason = "the single allowlisted reader")]
pub fn env_var(var: EnvVar) -> Option<std::ffi::OsString> {
    let name = match var {
        EnvVar::Home => "HOME",
    };
    std::env::var_os(name)
}

/// Startup step 4: lock all current and future memory so plaintext never
/// reaches swap. Fails closed if the memlock limit is below the floor or
/// the kernel refuses.
pub fn lock_memory() -> Result<(), HardenError> {
    // Raising the soft limit up to the hard limit needs no privilege.
    let hard = getrlimit(Resource::Memlock).maximum;
    let raised = Rlimit {
        current: hard,
        maximum: hard,
    };
    setrlimit(Resource::Memlock, raised).map_err(step_failed("setrlimit(RLIMIT_MEMLOCK)"))?;
    check_memlock_floor(hard)?;
    mlockall(MlockAllFlags::CURRENT | MlockAllFlags::FUTURE | MlockAllFlags::ONFAULT)
        .map_err(step_failed("mlockall"))
}

/// `None` means unlimited.
fn check_memlock_floor(hard_limit: Option<u64>) -> Result<(), HardenError> {
    match hard_limit {
        Some(hard_limit) if hard_limit < MEMLOCK_FLOOR => {
            Err(HardenError::MemlockTooLow { hard_limit })
        }
        _ => Ok(()),
    }
}

/// Takes only the location, never the payload, so note text cannot reach it.
fn write_panic_report(out: &mut impl Write, location: Option<&Location<'_>>) -> io::Result<()> {
    match location {
        Some(loc) => writeln!(
            out,
            "crystal: internal error at {}:{}",
            loc.file(),
            loc.line()
        ),
        None => writeln!(out, "crystal: internal error"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panic_report_is_location_only() {
        let loc = Location::caller();
        let mut out = Vec::new();
        write_panic_report(&mut out, Some(loc)).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("crystal: internal error at {}:{}\n", loc.file(), loc.line())
        );
    }

    // Changes the whole test process (dumpable, rlimit, umask) and this
    // thread (no_new_privs); all of that is harmless for the other tests.
    #[test]
    fn lock_down_process_applies_every_setting() {
        lock_down_process().unwrap();

        let core = rustix::process::getrlimit(Resource::Core);
        assert_eq!((core.current, core.maximum), (Some(0), Some(0)));
        assert_eq!(
            rustix::process::dumpable_behavior().unwrap(),
            DumpableBehavior::NotDumpable
        );
        assert!(rustix::thread::no_new_privs().unwrap());
        // umask can only be read by setting it; set the same value back.
        assert_eq!(umask(Mode::RWXG | Mode::RWXO), Mode::RWXG | Mode::RWXO);
    }

    #[test]
    fn memlock_floor_boundaries() {
        assert!(check_memlock_floor(None).is_ok());
        assert!(check_memlock_floor(Some(MEMLOCK_FLOOR)).is_ok());
        assert!(matches!(
            check_memlock_floor(Some(MEMLOCK_FLOOR - 1)),
            Err(HardenError::MemlockTooLow { .. })
        ));
        assert!(matches!(
            check_memlock_floor(Some(0)),
            Err(HardenError::MemlockTooLow { .. })
        ));
    }

    // Phase 1 acceptance: VmLck is non-zero. On a machine whose limit is
    // below the floor, Crystal must refuse instead; both outcomes are checked.
    #[test]
    fn lock_memory_locks_or_refuses() {
        let hard = getrlimit(Resource::Memlock).maximum;
        let result = lock_memory();
        if hard.is_some_and(|h| h < MEMLOCK_FLOOR) {
            assert!(matches!(result, Err(HardenError::MemlockTooLow { .. })));
        } else {
            result.unwrap();
            assert!(vm_locked_kib() > 0);
        }
    }

    fn vm_locked_kib() -> u64 {
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let line = status.lines().find(|l| l.starts_with("VmLck:")).unwrap();
        line.split_whitespace().nth(1).unwrap().parse().unwrap()
    }

    #[test]
    fn stray_fds_ignores_stdio_dots_and_own_fd() {
        let names: [&[u8]; 7] = [b".", b"..", b"0", b"1", b"2", b"3", b"7"];
        assert_eq!(stray_fds(names.into_iter(), 3), vec![7]);
        assert_eq!(stray_fds(names[..6].iter().copied(), 3), Vec::<i32>::new());
    }

    #[test]
    fn panic_report_without_location() {
        let mut out = Vec::new();
        write_panic_report(&mut out, None).unwrap();
        assert_eq!(out, b"crystal: internal error\n");
    }
}
