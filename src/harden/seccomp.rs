//! Startup step 9: the seccomp filter (docs/design/sandboxing.md,
//! section 6; decision 0059). An allowlist: every syscall Crystal makes
//! after this point has a row in docs/reference/syscalls.md, and any
//! other syscall, or an allowed one with other arguments, kills the
//! process. Applied after Landlock, before the passphrase prompt.

use seccompiler::{
    BackendError, BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition,
    SeccompFilter, SeccompRule, TargetArch,
};

use super::HardenError;

/// What happens on a syscall not allowed below: the whole process dies at
/// once (`SIGSYS`), with no handler and no chance to go on.
const MISMATCH: SeccompAction = SeccompAction::KillProcess;

/// Allowed with any arguments (x86_64 numbers, from `libc`). Landlock
/// limits which files `openat` and the others can reach.
const UNCONDITIONAL: &[i64] = &[
    // Files beneath the vault and state directories (vault::atomic_write,
    // public_file, generation, pin); reading keys and drawing (term).
    libc::SYS_openat,
    libc::SYS_read,
    libc::SYS_write,
    libc::SYS_close,
    libc::SYS_fstat,
    libc::SYS_newfstatat,
    libc::SYS_getdents64,
    libc::SYS_renameat2,
    libc::SYS_unlinkat,
    libc::SYS_fsync,
    // Waiting for keys and signals; returning from a signal handler.
    libc::SYS_ppoll,
    libc::SYS_rt_sigreturn,
    // Own pid for the suspend (term::TerminalGuard::suspend).
    libc::SYS_getpid,
    // Randomness and clocks.
    libc::SYS_getrandom,
    libc::SYS_clock_gettime,
    // Memory (musl's allocator under zalloc) and exit.
    libc::SYS_brk,
    libc::SYS_munmap,
    libc::SYS_exit_group,
];

/// The `ioctl` requests rustix makes on the terminal: `TCGETS2` (read the
/// settings), `TCSETS2` (raw mode), `TCSETSF2` (restore, discarding unread
/// keys), `TIOCGWINSZ` (window size) and `TCFLSH` (discard keys typed
/// before a prompt). Everything else, `TIOCSTI` and `TIOCLINUX` included,
/// kills. rustix falls back to `TCGETS` only after an error return, which
/// a kill never gives.
const IOCTL_REQUESTS: &[libc::Ioctl] = &[
    libc::TCGETS2,
    libc::TCSETS2,
    libc::TCSETSF2,
    libc::TIOCGWINSZ,
    libc::TCFLSH,
];

/// Argument `index` equals `value` in its low 32 bits, the part the
/// kernel reads for an `int` or `unsigned int` argument.
fn eq32(index: u8, value: u32) -> Result<SeccompCondition, BackendError> {
    let value = u64::from(value);
    SeccompCondition::new(index, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, value)
}

/// One rule: all `conditions` must hold.
fn rule(conditions: Vec<SeccompCondition>) -> Result<SeccompRule, BackendError> {
    SeccompRule::new(conditions)
}

/// The syscalls allowed only with certain arguments; within a syscall,
/// any one rule allows it.
fn conditional(own_pid: u32) -> Result<Vec<(i64, Vec<SeccompRule>)>, BackendError> {
    // Bit patterns, not values: ioctl requests are unsigned on the
    // kernel side, while musl declares them as `int`.
    #[expect(clippy::cast_sign_loss, reason = "ioctl requests are bit patterns")]
    let ioctl = IOCTL_REQUESTS
        .iter()
        .map(|&request| rule(vec![eq32(1, request as u32)?]))
        .collect::<Result<Vec<_>, _>>()?;
    // A second terminal fd on resume (std's File::try_clone): musl's
    // fcntl() makes F_DUPFD_CLOEXEC, then sets FD_CLOEXEC on the new fd
    // once more (for old kernels) and ignores the result. Debug builds
    // also check each closed fd with F_GETFD (std's
    // debug_assert_fd_is_open); release builds never do.
    let mut fcntl = vec![
        rule(vec![eq32(1, libc::F_DUPFD_CLOEXEC as u32)?])?,
        rule(vec![
            eq32(1, libc::F_SETFD as u32)?,
            eq32(2, libc::FD_CLOEXEC as u32)?,
        ])?,
    ];
    if cfg!(debug_assertions) {
        fcntl.push(rule(vec![eq32(1, libc::F_GETFD as u32)?])?);
    }
    let sigstop = libc::SIGSTOP as u32;
    // The suspend: SIGSTOP to ourselves only, never to another process.
    let kill = vec![rule(vec![eq32(0, own_pid)?, eq32(1, sigstop)?])?];
    // crypto::scrypt_policy reads the memlock limit of this process; it
    // may never set a limit (the new-limit pointer must be NULL).
    let memlock = libc::RLIMIT_MEMLOCK as u32;
    let prlimit = vec![rule(vec![
        eq32(0, 0)?,
        eq32(1, memlock)?,
        SeccompCondition::new(2, SeccompCmpArgLen::Qword, SeccompCmpOp::Eq, 0)?,
    ])?];
    // Memory for the allocator, never executable: no PROT_EXEC bit.
    let prot_exec = u64::from(libc::PROT_EXEC as u32);
    let mmap = vec![rule(vec![SeccompCondition::new(
        2,
        SeccompCmpArgLen::Dword,
        SeccompCmpOp::MaskedEq(prot_exec),
        0,
    )?])?];
    Ok(vec![
        (libc::SYS_ioctl, ioctl),
        (libc::SYS_fcntl, fcntl),
        (libc::SYS_kill, kill),
        (libc::SYS_prlimit64, prlimit),
        (libc::SYS_mmap, mmap),
    ])
}

/// The filter for a process whose pid is `own_pid`.
fn program(own_pid: u32) -> Result<BpfProgram, BackendError> {
    let rules = UNCONDITIONAL
        .iter()
        .map(|&nr| (nr, Vec::new()))
        .chain(conditional(own_pid)?)
        .collect();
    let filter = SeccompFilter::new(rules, MISMATCH, SeccompAction::Allow, TargetArch::x86_64)?;
    BpfProgram::try_from(filter)
}

/// Builds the filter and applies it to every thread, for good.
pub fn restrict() -> Result<(), HardenError> {
    let own_pid = rustix::process::getpid().as_raw_pid().unsigned_abs();
    let program = program(own_pid).map_err(|e| HardenError::Seccomp(e.into()))?;
    seccompiler::apply_filter_all_threads(&program).map_err(HardenError::Seccomp)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every rule is valid and the filter compiles; 23 syscalls in all,
    /// the number docs/reference/syscalls.md lists.
    #[test]
    fn the_filter_compiles_with_23_syscalls() {
        assert!(program(1234).is_ok());
        let conditional = conditional(1234).expect("rules are valid");
        assert_eq!(UNCONDITIONAL.len() + conditional.len(), 23);
    }
}
