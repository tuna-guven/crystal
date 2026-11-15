//! Startup step 9: the seccomp filter (docs/design/sandboxing.md,
//! section 6; decision 0059). An allowlist: every syscall Crystal makes
//! after this point has a row in docs/reference/syscalls.md, and any
//! other syscall kills the process. Applied after Landlock, before the
//! passphrase prompt.

use std::collections::BTreeMap;

use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, SeccompRule, TargetArch};

use super::HardenError;

/// What happens on a syscall not in [`ALLOWED`]: the whole process dies
/// at once (`SIGSYS`), with no handler and no chance to go on.
const MISMATCH: SeccompAction = SeccompAction::KillProcess;

/// Syscalls allowed after the sandbox, by number (x86_64, from `libc`).
/// Argument rules (ioctl requests, own-pid kill, no PROT_EXEC) come in
/// step S5.
const ALLOWED: &[i64] = &[
    // Files beneath the vault and state directories (vault::atomic_write,
    // public_file, generation, pin, the sync-conflict listing).
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
    // Terminal, signals, suspend (term).
    libc::SYS_ppoll,
    libc::SYS_ioctl,
    libc::SYS_fcntl,
    libc::SYS_getpid,
    libc::SYS_kill,
    libc::SYS_rt_sigreturn,
    // Randomness, clocks, scrypt's memory check (crypto::scrypt_policy).
    libc::SYS_getrandom,
    libc::SYS_prlimit64,
    libc::SYS_clock_gettime,
    // Memory and the Rust runtime (musl malloc, std).
    libc::SYS_brk,
    libc::SYS_mmap,
    libc::SYS_munmap,
    libc::SYS_exit_group,
];

/// Builds the filter and applies it to every thread, for good.
pub fn restrict() -> Result<(), HardenError> {
    let rules: BTreeMap<i64, Vec<SeccompRule>> =
        ALLOWED.iter().map(|&nr| (nr, Vec::new())).collect();
    let filter = SeccompFilter::new(rules, MISMATCH, SeccompAction::Allow, TargetArch::x86_64)
        .map_err(|e| HardenError::Seccomp(e.into()))?;
    let program = BpfProgram::try_from(filter).map_err(|e| HardenError::Seccomp(e.into()))?;
    seccompiler::apply_filter_all_threads(&program).map_err(HardenError::Seccomp)
}
