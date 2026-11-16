//! Startup step 8: Landlock (docs/design/sandboxing.md, section 5;
//! decisions 0010 and 0059). After this, the process reads and writes
//! files only beneath the vault directory and the state directory, opens
//! no TCP connection, reaches no pathname UNIX socket (D-Bus, Wayland) and
//! no abstract one, and signals no process outside its own domain. Every
//! fd opened before (the terminal, the signal pipes) keeps working:
//! Landlock checks at open time.

use std::os::fd::BorrowedFd;

use ::landlock::{
    ABI, Access, AccessFs, AccessNet, BitFlags, CompatLevel, Compatible, PathBeneath,
    RestrictSelfAttr, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus, Scope,
};

use super::HardenError;

/// ABI 9 brings `RESOLVE_UNIX`, which blocks pathname UNIX sockets; the
/// scopes need 6. Below it Crystal refuses to start (decision 0010).
pub const REQUIRED_ABI: u8 = 9;
const REQUIRED: ABI = ABI::V9;

/// What Crystal may do beneath the vault and the state directory: read,
/// list, write and truncate files (always together), create a file (the
/// atomic writer's temp file) and remove one (the rename over the target,
/// a stale temp file). Never `REFER` (the temp file is in the target's own
/// directory), `EXECUTE`, `MAKE_DIR`, `REMOVE_DIR` (only `init` and `rm`
/// do that, outside the sandbox), `IOCTL_DEV` or `RESOLVE_UNIX`.
fn private_dir_rights() -> BitFlags<AccessFs> {
    AccessFs::ReadFile
        | AccessFs::ReadDir
        | AccessFs::WriteFile
        | AccessFs::Truncate
        | AccessFs::MakeReg
        | AccessFs::RemoveFile
}

/// Restricts the process for good. `vault` and `state` are the directory
/// fds startup step 7 opened (no path is resolved again); without `state`
/// (`crystal check-code`, which changes nothing) only the vault is
/// reachable. Every right up
/// to ABI 9 is handled, so everything not granted below is denied; TCP
/// bind and connect are handled with no rule, so they are always denied.
/// Hard-requirement mode: a right the kernel cannot enforce is an error,
/// never silently dropped.
pub fn restrict(vault: BorrowedFd<'_>, state: Option<BorrowedFd<'_>>) -> Result<(), HardenError> {
    // Only compatibility checks run here, no syscall but the ABI query:
    // a failure means the kernel is older than ABI 9 or Landlock is off.
    let handled = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(REQUIRED))
        .and_then(|ruleset| ruleset.handle_access(AccessNet::from_all(REQUIRED)))
        .and_then(|ruleset| ruleset.scope(Scope::from_all(REQUIRED)))
        .map_err(|_| HardenError::LandlockTooOld)?;
    let status = handled
        .create()
        .and_then(|ruleset| ruleset.add_rule(PathBeneath::new(vault, private_dir_rights())))
        .and_then(|ruleset| match state {
            Some(state) => ruleset.add_rule(PathBeneath::new(state, private_dir_rights())),
            None => Ok(ruleset),
        })
        // TSYNC: every thread. Crystal has one; defense in depth.
        .and_then(|ruleset| ruleset.all_threads(true))
        .and_then(|ruleset| ruleset.restrict_self())
        .map_err(HardenError::Landlock)?;
    // Hard-requirement mode should make anything else impossible; check
    // anyway, so a crate bug cannot leave Crystal running unconfined.
    if status.ruleset != RulesetStatus::FullyEnforced || !status.all_threads {
        return Err(HardenError::LandlockNotEnforced);
    }
    Ok(())
}
