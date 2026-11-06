//! Terminal guard: alternate screen, raw mode, clearing on exit, panic and
//! signals. Note text never reaches the main screen.

use std::fmt;
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::ops::RangeInclusive;
use std::os::fd::OwnedFd;
use std::sync::{Mutex, PoisonError, TryLockError};

use rustix::event::{PollFd, PollFlags, poll};
use rustix::fs::{CWD, Mode, OFlags, openat};
use rustix::io::Errno;
use rustix::pipe::{PipeFlags, pipe_with};
use rustix::process::{Signal, getpid, kill_process};
use rustix::termios::{OptionalActions, Termios, isatty, tcgetattr, tcsetattr};

/// Enter the alternate screen, clear it, cursor to top left.
const ENTER_SCREEN: &[u8] = b"\x1b[?1049h\x1b[2J\x1b[H";
/// Clear the alternate screen while still on it, then leave it, so none of
/// its content survives in terminals that keep it around.
const LEAVE_SCREEN: &[u8] = b"\x1b[2J\x1b[?1049l";

/// The controlling terminal, opened directly. Redirected stdin or stdout can
/// then neither feed input (a passphrase) nor capture output (the UI).
pub struct Tty {
    file: File,
}

/// Holds only an errno, never any data.
#[derive(Debug)]
pub enum TtyError {
    Open(Errno),
    NotATerminal,
}

impl fmt::Display for TtyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open(errno) => write!(
                f,
                "cannot open /dev/tty ({errno}); Crystal must run in an interactive terminal"
            ),
            Self::NotATerminal => write!(f, "/dev/tty is not a terminal"),
        }
    }
}

impl std::error::Error for TtyError {}

impl Tty {
    /// Startup step 5: require a real terminal and open it directly.
    pub fn open() -> Result<Self, TtyError> {
        // CLOEXEC: the age plugin must not inherit the terminal.
        // NOCTTY: opening never makes it our controlling terminal.
        let flags = OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NOFOLLOW;
        let fd = openat(CWD, "/dev/tty", flags, Mode::empty()).map_err(TtyError::Open)?;
        if !isatty(&fd) {
            return Err(TtyError::NotATerminal);
        }
        Ok(Self {
            file: File::from(fd),
        })
    }
}

/// What restoring the terminal needs. Set by `TerminalGuard::enter` and
/// taken by the first `restore_terminal` call, from the guard's Drop or from
/// the panic hook, whichever runs first.
struct Saved {
    tty: File,
    original: Termios,
}

static SAVED: Mutex<Option<Saved>> = Mutex::new(None);

/// Raw mode and the alternate screen for as long as it lives. Dropping it,
/// including during a panic unwind, clears and leaves the alternate screen
/// and restores the original terminal settings.
pub struct TerminalGuard {
    tty: File,
}

/// Holds only an errno or an error kind, never any data.
#[derive(Debug)]
pub enum TermError {
    Dup(io::ErrorKind),
    GetAttr(Errno),
    SetAttr(Errno),
    Write(io::ErrorKind),
    Pipe(Errno),
    Signal(io::ErrorKind),
    Poll(Errno),
    Stop(Errno),
}

impl fmt::Display for TermError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dup(kind) => write!(f, "cannot duplicate the terminal fd ({kind})"),
            Self::GetAttr(errno) => write!(f, "cannot read terminal settings ({errno})"),
            Self::SetAttr(errno) => write!(f, "cannot set raw mode ({errno})"),
            Self::Write(kind) => write!(f, "cannot write to the terminal ({kind})"),
            Self::Pipe(errno) => write!(f, "cannot create the signal pipe ({errno})"),
            Self::Signal(kind) => write!(f, "cannot install signal handlers ({kind})"),
            Self::Poll(errno) => write!(f, "cannot wait for input ({errno})"),
            Self::Stop(errno) => write!(f, "cannot suspend ({errno})"),
        }
    }
}

impl std::error::Error for TermError {}

impl TerminalGuard {
    /// Raw mode: no echo, no line buffering, no signals from keys; each key
    /// arrives as pressed. Then the alternate screen.
    pub fn enter(tty: Tty) -> Result<Self, TermError> {
        // The guard exists before anything changes, so its Drop restores
        // the terminal whichever step of engage() fails.
        let mut guard = Self { tty: tty.file };
        guard.engage()?;
        Ok(guard)
    }

    /// Saves the current settings, enters raw mode, then the alternate
    /// screen. Also used on resume after a suspend: the shell may have
    /// changed the settings meanwhile, so they are read again.
    fn engage(&mut self) -> Result<(), TermError> {
        // Duplicate before raw mode, so a failure here leaves the terminal
        // untouched.
        let restore_tty = self.tty.try_clone().map_err(|e| TermError::Dup(e.kind()))?;
        let original = tcgetattr(&self.tty).map_err(TermError::GetAttr)?;
        let mut raw = original.clone();
        raw.make_raw();
        tcsetattr(&self.tty, OptionalActions::Now, &raw).map_err(TermError::SetAttr)?;
        // Single-threaded: nothing else can hold the lock here.
        *SAVED.lock().unwrap_or_else(PoisonError::into_inner) = Some(Saved {
            tty: restore_tty,
            original,
        });
        self.tty
            .write_all(ENTER_SCREEN)
            .map_err(|e| TermError::Write(e.kind()))
    }

    /// Ctrl+Z or SIGTSTP: restore the terminal, stop, and re-engage once
    /// resumed. Phase 5: lock first (zeroize notes, forget the identity)
    /// and ask for the passphrase again after resume.
    fn suspend(&mut self) -> Result<(), TermError> {
        restore_terminal();
        // SIGSTOP cannot be caught. kill() on ourselves returns only after
        // a SIGCONT (fg or bg) has resumed the process.
        kill_process(getpid(), Signal::STOP).map_err(TermError::Stop)?;
        self.engage()
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Clears and leaves the alternate screen and restores the saved settings,
/// at most once. Called by the panic hook before it prints, so the message
/// lands on the normal screen. Never blocks and never panics: try_lock, so
/// a panic that starts inside this function skips instead of deadlocking.
/// A poisoned lock is still used; restoring the terminal matters more.
pub fn restore_terminal() {
    let saved = match SAVED.try_lock() {
        Ok(mut saved) => saved.take(),
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().take(),
        Err(TryLockError::WouldBlock) => return,
    };
    let Some(mut saved) = saved else {
        return;
    };
    // Errors are ignored: there is nothing better to do. Flush (TCSAFLUSH)
    // also discards unread input, so keys typed into Crystal never reach
    // the shell after exit.
    let _ = saved.tty.write_all(LEAVE_SCREEN);
    let _ = tcsetattr(&saved.tty, OptionalActions::Flush, &saved.original);
}

/// Every catchable signal whose default action kills Crystal without
/// running Drop: the terminal would stay raw on the alternate screen, and
/// later notes would not be zeroized. Each one ends Crystal through the
/// normal exit path instead. Not listed, so left at their defaults:
/// - SIGKILL, SIGSTOP: cannot be caught.
/// - SIGILL, SIGFPE, SIGSEGV, SIGBUS, SIGTRAP, SIGSYS, SIGABRT: raised by a
///   fault or by abort(); returning from a handler would rerun the fault.
/// - SIGTTIN, SIGTTOU: the kernel raises them again each time it restarts
///   the terminal call that caused them, so a handler would loop.
/// - SIGPIPE: the Rust runtime ignores it; writes return EPIPE instead.
const EXIT_SIGNALS: [Signal; 14] = [
    Signal::TERM,
    Signal::HUP,
    Signal::INT,
    Signal::QUIT,
    Signal::USR1,
    Signal::USR2,
    Signal::ALARM,
    Signal::VTALARM,
    Signal::PROF,
    Signal::XCPU,
    Signal::XFSZ,
    Signal::IO,
    Signal::POWER,
    Signal::STKFLT,
];

/// Linux real-time signals on x86_64, 32 to 64 (the kernel's SIGRTMIN and
/// _NSIG). Their default action also kills the process.
const RT_SIGNALS: RangeInclusive<i32> = 32..=64;

/// The C library keeps the lowest real-time signals for itself and refuses
/// handlers for them with EINVAL (musl: 32 to 34; glibc: 32 and 33). Those
/// stay uncatchable, like SIGKILL; any other failure is an error.
const LIBC_RESERVED_MAX: i32 = 34;

/// Ctrl+Z arrives as this byte in raw mode instead of as SIGTSTP.
const CTRL_Z: u8 = 0x1a;

/// Self-pipes for signals: one for exit, one for suspend (SIGTSTP). Each
/// handler only writes one byte to its pipe (async-signal-safe); the main
/// loop sees it. No thread, no socketpair (both denied by the sandbox).
pub struct Signals {
    exit: OwnedFd,
    suspend: OwnedFd,
}

impl Signals {
    /// Register before entering the terminal guard, so no signal can kill
    /// or stop the process while the terminal is raw.
    pub fn register() -> Result<Self, TermError> {
        let signal_failed = |e: io::Error| TermError::Signal(e.kind());

        let (exit, exit_write) = self_pipe()?;
        for signal in EXIT_SIGNALS {
            on_signal(signal.as_raw(), &exit_write).map_err(signal_failed)?;
        }
        for signal in RT_SIGNALS {
            match on_signal(signal, &exit_write) {
                Ok(()) => {}
                Err(e) if signal <= LIBC_RESERVED_MAX && e.kind() == ErrorKind::InvalidInput => {}
                Err(e) => return Err(signal_failed(e)),
            }
        }

        let (suspend, suspend_write) = self_pipe()?;
        on_signal(Signal::TSTP.as_raw(), &suspend_write).map_err(signal_failed)?;

        Ok(Self { exit, suspend })
    }

    /// Empties the suspend pipe, so one SIGTSTP suspends once. The read end
    /// is non-blocking, so this stops at "would block".
    fn drain_suspend(&self) {
        let mut byte = [0u8; 16];
        while matches!(rustix::io::read(&self.suspend, &mut byte), Ok(n) if n > 0) {}
    }
}

/// A non-blocking pipe: (read end, write end). The write end is only
/// cloned for handlers and then dropped.
fn self_pipe() -> Result<(OwnedFd, OwnedFd), TermError> {
    pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK).map_err(TermError::Pipe)
}

/// From now on, `signal` writes one byte to `pipe`. signal-hook owns the
/// fd it is given, so each signal gets its own copy of the write end.
fn on_signal(signal: i32, pipe: &OwnedFd) -> io::Result<()> {
    signal_hook::low_level::pipe::register(signal, pipe.try_clone()?).map(drop)
}

impl TerminalGuard {
    /// Phase 1 placeholder for the TUI loop (replaced in Phase 3): Ctrl+Z
    /// or SIGTSTP suspends, any other key or an exit signal returns. Keys
    /// are read and discarded, never stored or shown. Exit wins over
    /// suspend, suspend over keys.
    pub fn run_until_exit(&mut self, signals: &Signals) -> Result<(), TermError> {
        loop {
            let mut fds = [
                PollFd::new(&self.tty, PollFlags::IN),
                PollFd::new(&signals.exit, PollFlags::IN),
                PollFd::new(&signals.suspend, PollFlags::IN),
            ];
            match poll(&mut fds, None) {
                Ok(_) => {}
                Err(Errno::INTR) => continue,
                Err(errno) => return Err(TermError::Poll(errno)),
            }
            let [key, exit, suspend] = fds.map(|fd| !fd.revents().is_empty());
            if exit {
                return Ok(());
            }
            if suspend {
                signals.drain_suspend();
                self.suspend()?;
                continue;
            }
            if key {
                let mut keys = [0u8; 64];
                let n = self.tty.read(&mut keys).unwrap_or(0);
                if keys[..n].contains(&CTRL_Z) {
                    self.suspend()?;
                    continue;
                }
                return Ok(());
            }
        }
    }
}
