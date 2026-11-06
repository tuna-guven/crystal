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
use rustix::termios::{
    OptionalActions, QueueSelector, Termios, isatty, tcflush, tcgetattr, tcsetattr,
};
use zeroize::{Zeroize, Zeroizing};

use crate::crypto::Passphrase;

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

/// The longest passphrase accepted, in bytes. The buffer is allocated at
/// this size up front and a longer input is refused, so it never grows
/// (growing would copy the passphrase and free the old buffer unwiped).
pub const MAX_PASSPHRASE: usize = 1024;

/// Clear the alternate screen, cursor to top left.
const CLEAR_SCREEN: &[u8] = b"\x1b[2J\x1b[H";

const CTRL_C: u8 = 0x03;
const BACKSPACE: u8 = 0x08;
const CTRL_U: u8 = 0x15;
const ESC: u8 = 0x1b;
const DELETE: u8 = 0x7f;

/// Why a passphrase prompt ended without a passphrase. Fixed text and
/// errnos only, never any typed byte.
#[derive(Debug, PartialEq, Eq)]
pub enum PromptError {
    /// Ctrl+C, Esc, or an exit signal.
    Cancelled,
    /// More than `MAX_PASSPHRASE` bytes.
    TooLong,
    /// Not valid UTF-8 (age's scrypt needs text).
    NotText,
    /// The two entries of `read_new_passphrase` differ.
    Mismatch,
    /// The terminal went away (hangup or end of input).
    Closed,
    Flush(Errno),
    Poll(Errno),
    Read(ErrorKind),
    Write(ErrorKind),
}

impl fmt::Display for PromptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "passphrase entry cancelled"),
            Self::TooLong => write!(f, "passphrase longer than {MAX_PASSPHRASE} bytes"),
            Self::NotText => write!(f, "passphrase is not valid UTF-8 text"),
            Self::Mismatch => write!(f, "the passphrases do not match"),
            Self::Closed => write!(f, "the terminal closed during passphrase entry"),
            Self::Flush(errno) => write!(f, "cannot discard earlier input ({errno})"),
            Self::Poll(errno) => write!(f, "cannot wait for input ({errno})"),
            Self::Read(kind) => write!(f, "cannot read the terminal ({kind})"),
            Self::Write(kind) => write!(f, "cannot write to the terminal ({kind})"),
        }
    }
}

impl std::error::Error for PromptError {}

/// What one input byte did to the line.
#[derive(Debug, PartialEq, Eq)]
enum Feed {
    More,
    Enter,
    Cancel,
    TooLong,
}

/// The typed passphrase, in a buffer of fixed capacity that is wiped on
/// drop. Deleted bytes are zeroed at once, not just cut off, so the spare
/// capacity never holds old input either.
struct LineEditor(Zeroizing<Vec<u8>>);

impl LineEditor {
    fn new() -> Self {
        Self(Zeroizing::new(Vec::with_capacity(MAX_PASSPHRASE)))
    }

    fn feed(&mut self, byte: u8) -> Feed {
        match byte {
            // Raw mode turns off CR-to-NL, so Enter arrives as CR. An
            // empty line is ignored rather than refused.
            b'\r' | b'\n' if self.0.is_empty() => Feed::More,
            b'\r' | b'\n' => Feed::Enter,
            // One whole UTF-8 character: continuation bytes (10xxxxxx),
            // then the byte that starts it.
            DELETE | BACKSPACE => {
                let mut len = self.0.len();
                while len > 0 {
                    len -= 1;
                    if self.0[len] & 0xc0 != 0x80 {
                        break;
                    }
                }
                self.truncate(len);
                Feed::More
            }
            CTRL_U => {
                self.truncate(0);
                Feed::More
            }
            // Esc also starts arrow and function keys; any of them cancels.
            CTRL_C | ESC => Feed::Cancel,
            // Other control bytes (Tab, Ctrl+Z, ...) are ignored.
            0..0x20 => Feed::More,
            _ if self.0.len() == MAX_PASSPHRASE => Feed::TooLong,
            _ => {
                self.0.push(byte);
                Feed::More
            }
        }
    }

    fn truncate(&mut self, len: usize) {
        self.0[len..].zeroize();
        self.0.truncate(len);
    }

    /// The buffer itself becomes the passphrase: no copy is made.
    fn finish(self) -> Result<Passphrase, PromptError> {
        Passphrase::new(self.0).ok_or(PromptError::NotText)
    }
}

#[cfg_attr(
    not(debug_assertions),
    expect(dead_code, reason = "--init (#14) and unlock (#15)")
)]
impl TerminalGuard {
    /// Asks for a passphrase on the alternate screen. Nothing is echoed,
    /// not even asterisks, which would show the length. Enter accepts,
    /// Backspace deletes a character, Ctrl+U clears, Ctrl+C or Esc (or an
    /// exit signal) cancels. Keys typed before the prompt appeared are
    /// discarded: the terminal echoed them while it was not yet raw.
    pub fn read_passphrase(
        &mut self,
        signals: &Signals,
        prompt: &'static str,
    ) -> Result<Passphrase, PromptError> {
        tcflush(&self.tty, QueueSelector::IFlush).map_err(PromptError::Flush)?;
        self.show(prompt.as_bytes())?;
        let result = self.read_line(signals);
        self.show(b"")?;
        result
    }

    /// For --init: asks twice and accepts only matching entries, compared
    /// in constant time.
    pub fn read_new_passphrase(
        &mut self,
        signals: &Signals,
        prompt: &'static str,
        repeat: &'static str,
    ) -> Result<Passphrase, PromptError> {
        let first = self.read_passphrase(signals, prompt)?;
        let second = self.read_passphrase(signals, repeat)?;
        if first.matches(&second) {
            Ok(first)
        } else {
            Err(PromptError::Mismatch)
        }
    }

    /// Clears the alternate screen and writes `text` at the top left.
    fn show(&mut self, text: &[u8]) -> Result<(), PromptError> {
        let write = |tty: &mut File, bytes| tty.write_all(bytes);
        write(&mut self.tty, CLEAR_SCREEN)
            .and_then(|()| write(&mut self.tty, text))
            .map_err(|e| PromptError::Write(e.kind()))
    }

    /// Reads keys until Enter or a cancel. Each read lands in a small
    /// stack buffer that is wiped on drop; bytes after Enter in the same
    /// read are discarded with it.
    fn read_line(&mut self, signals: &Signals) -> Result<Passphrase, PromptError> {
        let mut line = LineEditor::new();
        let mut chunk = Zeroizing::new([0u8; 64]);
        loop {
            let mut fds = [
                PollFd::new(&self.tty, PollFlags::IN),
                PollFd::new(&signals.exit, PollFlags::IN),
            ];
            match poll(&mut fds, None) {
                Ok(_) => {}
                Err(Errno::INTR) => continue,
                Err(errno) => return Err(PromptError::Poll(errno)),
            }
            let [key, exit] = fds.map(|fd| !fd.revents().is_empty());
            if exit {
                return Err(PromptError::Cancelled);
            }
            if !key {
                continue;
            }
            let n = match self.tty.read(&mut chunk[..]) {
                Ok(0) => return Err(PromptError::Closed),
                Ok(n) => n,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                // A hung-up terminal reads as EIO.
                Err(e) if e.raw_os_error() == Some(Errno::IO.raw_os_error()) => {
                    return Err(PromptError::Closed);
                }
                Err(e) => return Err(PromptError::Read(e.kind())),
            };
            for &byte in &chunk[..n] {
                match line.feed(byte) {
                    Feed::More => {}
                    Feed::Enter => return line.finish(),
                    Feed::Cancel => return Err(PromptError::Cancelled),
                    Feed::TooLong => return Err(PromptError::TooLong),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(keys: &[u8]) -> (LineEditor, Vec<Feed>) {
        let mut line = LineEditor::new();
        let feeds = keys.iter().map(|b| line.feed(*b)).collect();
        (line, feeds)
    }

    fn line_after(keys: &[u8]) -> Vec<u8> {
        typed(keys).0.0.to_vec()
    }

    #[test]
    fn editing_keys() {
        assert_eq!(line_after(b"hunter2"), b"hunter2");
        assert_eq!(line_after(b"hunter2xx\x7f\x7f"), b"hunter2");
        assert_eq!(line_after(b"ab\x08c"), b"ac");
        assert_eq!(line_after(b"garbage\x15pw"), b"pw");
        assert_eq!(line_after(b"\x7f\x7fa"), b"a");
        assert_eq!(line_after(b"a\tb\x1ac"), b"abc");
        // Backspace removes a whole character: 2, 3 and 4 bytes.
        assert_eq!(line_after("pä\x7f".as_bytes()), b"p");
        assert_eq!(line_after("p€\x7f".as_bytes()), b"p");
        assert_eq!(line_after("p😀\x7fq".as_bytes()), b"pq");
    }

    #[test]
    fn enter_and_cancel() {
        let (_, feeds) = typed(b"\rab\r");
        assert_eq!(feeds[0], Feed::More, "an empty Enter is ignored");
        assert_eq!(feeds[3], Feed::Enter);
        assert_eq!(typed(b"\n").1, [Feed::More]);
        assert_eq!(typed(b"a\n").1[1], Feed::Enter);
        assert_eq!(typed(b"a\x03").1[1], Feed::Cancel);
        assert_eq!(typed(b"a\x1b").1[1], Feed::Cancel);
    }

    #[test]
    fn finish_requires_utf8() {
        assert!(typed(b"pw").0.finish().is_ok());
        assert_eq!(
            typed(b"\xff\xfe").0.finish().err(),
            Some(PromptError::NotText)
        );
    }

    // Acceptance: the buffer never reallocates. The 1025th byte is
    // refused; the allocation stays the same one throughout.
    #[test]
    fn buffer_never_reallocates() {
        let mut line = LineEditor::new();
        let (ptr, cap) = (line.0.as_ptr(), line.0.capacity());
        assert_eq!(cap, MAX_PASSPHRASE);
        for _ in 0..MAX_PASSPHRASE {
            assert_eq!(line.feed(b'a'), Feed::More);
        }
        assert_eq!(line.feed(b'a'), Feed::TooLong);
        assert_eq!((line.0.as_ptr(), line.0.capacity()), (ptr, cap));
        let pass = line.finish().ok().unwrap();
        assert_eq!(pass.len_for_test(), MAX_PASSPHRASE);
    }

    // Deleted bytes are zeroed in the spare capacity right away, not only
    // when the buffer is dropped.
    #[test]
    fn deleted_bytes_are_zeroed_at_once() {
        let (mut line, _) = typed(b"secret\x7f\x7f");
        assert_eq!(line.0[..], *b"secr");
        assert_eq!(written_prefix(&mut line, 6), b"secr\0\0");
        line.feed(CTRL_U);
        assert_eq!(written_prefix(&mut line, 6), [0u8; 6]);
    }

    /// The first `n` bytes of the line's allocation, also past its length.
    /// Call only with `n` no larger than the longest the line has been:
    /// bytes beyond that were never written and are uninitialized.
    #[allow(unsafe_code, reason = "test-only check of wiped memory")]
    fn written_prefix(line: &mut LineEditor, n: usize) -> Vec<u8> {
        assert!(n <= line.0.capacity());
        // SAFETY:
        // - The allocation is owned by `line` and outlives this read:
        //   truncate keeps (does not free) the buffer.
        // - Each of the first `n` bytes was initialized: `feed` pushed a
        //   byte there, and truncate's zeroize later wrote 0 over it.
        // - The pointer comes from the Vec, so its provenance covers the
        //   whole allocation, not only the current length.
        unsafe { std::slice::from_raw_parts(line.0.as_ptr(), n).to_vec() }
    }
}
