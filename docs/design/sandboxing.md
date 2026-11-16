# Crystal Sandboxing Design

Status: Built (Phase 6, 2026-11-16): sections 5 to 7 and 9 describe the code; section 8 (bubblewrap) is still to be built (roadmap S10).
Scope: Process confinement for the `crystal` binary (offline, age-encrypted Markdown notes TUI)
Target platform: secureblue (Fedora Atomic, SELinux enforcing). Tested 2026-11-16 on kernel 7.3.3 (Landlock ABI 11). The `landlock` crate (0.4.7, the newest release) supports up to ABI 9, which is what Crystal uses (section 5.1).

## 1. Threat model summary

- Crystal handles decrypted notes and age identities in memory.
- Crystal never needs network access or IPC with other processes, and never calls `execve`: the sandbox is entered before the passphrase prompt ([decision 0059](../decisions.md#0059)), so exec is denied for the whole unlock and session. An age plugin (YubiKey or TPM, Phase 8) would need exec; how such a vault unlocks is decided then, without weakening the sandbox.
- Goal: a memory-safety bug, malicious Markdown, or a compromised dependency must not be able to read files outside the vault, exfiltrate data, or affect other processes.
- Defense in depth: every layer below must stand on its own. No layer may assume another one is active.

## 2. Layers

| # | Layer | Enforced by | Applies when |
|---|-------|-------------|--------------|
| 1 | Process hardening | `crystal` (prctl, rlimit, mlockall) | Always |
| 2 | Landlock (ABI 9 via the `landlock` crate) | `crystal` | Always; no fallback (5.4) |
| 3 | seccomp-bpf | `crystal` | Always |
| 4 | SELinux domain `crystal_t` | Kernel policy module | Installed via policy package |
| 5 | Bubblewrap | Launcher script | Optional outer wrapper |

## 3. Startup sequence (MUST follow this order)

Matches the [startup order](startup.md).

1. Process hardening (section 4; [startup](startup.md) steps 1 to 4).
2. Open `/dev/tty` directly and require a real terminal (step 5).
3. Parse arguments (step 6): a vault name, not a path. Open `~/.local/share/crystal` and then `NAME` relative to it ([decision 0032](../decisions.md#0032)), and the state directory, as `O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC` fds, and read `VERSION` (step 7). Paths are not canonicalized: the fds are what Crystal uses, and the vault and state fds are reused as the Landlock rule fds.
4. Enter raw mode and the alternate screen on the `/dev/tty` fd.
5. Apply Landlock (section 5; step 8).
6. Apply the seccomp filter (section 6; step 9). Requires no_new_privs, which step 1 already set.
7. Unlock, inside the sandbox ([decision 0059](../decisions.md#0059)): read the passphrase in raw mode from `/dev/tty` and decrypt `identity.age` (step 10). `crystal recover` and `crystal check-code` enter the sandbox the same way before their prompt; `init` and `rm` never do.
8. Decrypt the vault, build the indexes, start the shell (step 11).

Rules:
- Crystal is single-threaded (CLAUDE.md: no threads unless approved). Backups are made by external tools ([decision 0036](../decisions.md#0036)), never by Crystal.
- MUST NOT continue to step 7 if steps 1, 5 or 6 return an error. There are no fallbacks and no override flag: if any layer cannot be applied in full, Crystal refuses to start.
- All sandbox code lives in the `harden` module (`src/harden/landlock.rs`, `src/harden/seccomp.rs`) and is called from one function: `harden::enter_sandbox(vault, state) -> Result<(), HardenError>`, given the vault and state directory fds of step 3; Landlock first, then seccomp. `crystal unlock` and `crystal recover` pass both; `crystal check-code`, which reads and changes nothing, passes no state directory, so Landlock grants it the vault alone (`state` is an `Option`). `recover` and `check-code` call it right before the recovery code prompt (step S8; test `recover_and_check_code_wait_inside_the_sandbox`, mutation-checked for each command).

## 4. Process hardening

Module: `harden` ([startup](startup.md) steps 1 to 4; already implemented in Phase 1).

- Panic hook prints the source location only, after restoring the terminal.
- `setrlimit(RLIMIT_CORE, 0)` soft and hard: no core dumps containing plaintext.
- `prctl(PR_SET_DUMPABLE, 0)`: blocks ptrace and `/proc/<pid>/mem` reads from same-user processes.
- `prctl(PR_SET_NO_NEW_PRIVS, 1)`: set this early, so Landlock and seccomp need nothing more.
- `umask(0o077)`.
- Environment read only through `harden::env_var` (allowlist; enforced by clippy).
- `mlockall(MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT)` is mandatory, with a 512 MiB `RLIMIT_MEMLOCK` floor ([decision 0026](../decisions.md#0026)); Crystal refuses to start without it.
- Secrets: note text only in `NoteText` (`Zeroizing<String>`); the vault identity in a `Box` whose key wipes itself on drop ([decision 0013](../decisions.md#0013)); the `zalloc` global allocator zeroes every freed block.

## 5. Landlock

Module: `src/harden/landlock.rs`. Crate: `landlock`.

### 5.1 Implementation note

The `landlock` crate (0.4.7) exposes ABI up to V9 plus the `TSYNC` restrict flag. Crystal uses the crate only; no raw Landlock syscalls.

- A raw FFI layer would need `unsafe` in the crystal crate, which CLAUDE.md forbids (`unsafe` only in `crates/zalloc`).
- What ABI 10 adds is not needed: UDP rights (seccomp already denies `socket()` outright) and the `quiet_*` fields (Crystal wants every denial logged).
- The `LANDLOCK_RESTRICT_SELF_NO_NEW_PRIVS` flag an earlier draft assumed for ABI 11 does not exist in the kernel headers checked (kernel 7.2.9). Not needed: no_new_privs is set by `prctl` in section 4.
- Take every constant from the crate or from `/usr/include/linux/landlock.h`. MUST NOT hardcode values from memory.
- Revisit when the crate supports a newer ABI.

### 5.2 Ruleset

Query the ABI first (`LANDLOCK_CREATE_RULESET_VERSION`, done by the crate). Build the ruleset with `CompatLevel::HardRequirement`, so any right the running kernel cannot enforce is an error, never silently dropped.

Handled (denied unless a rule allows it):
- `handled_access_fs`: every FS right up to ABI 9, including `RESOLVE_UNIX` (blocks connecting to D-Bus, Wayland and other pathname UNIX sockets).
- `handled_access_net`: `BIND_TCP`, `CONNECT_TCP`. No net rules are added, so all TCP bind and connect is denied. UDP rights are ABI 10 and not available through the crate; seccomp denies `socket()` instead.
- `scoped`: `ABSTRACT_UNIX_SOCKET`, `SIGNAL`.

Rules (path beneath, from the directory fds opened at startup):

| Path | Rights |
|------|--------|
| The opened vault, `~/.local/share/crystal/NAME` (not all of `~/.local/share/crystal`) | READ_FILE, READ_DIR, WRITE_FILE, TRUNCATE, MAKE_REG, REMOVE_FILE |
| State dir (`~/.local/state/crystal`) | READ_FILE, READ_DIR, WRITE_FILE, TRUNCATE, MAKE_REG, REMOVE_FILE |
| Extra read-only paths | Only those proven necessary in log mode (section 6), e.g. `/etc/localtime` |

Notes:
- Always grant `WRITE_FILE` and `TRUNCATE` together (kernel docs recommendation).
- Atomic save MUST write the temp file in the same directory as the target, then `rename`. This avoids needing `REFER`. Replacing the target needs `REMOVE_FILE` and `MAKE_REG` on that directory.
- Do not grant `REFER`.
- No `REMOVE_DIR` and no `MAKE_DIR` anywhere: `crystal init` and `crystal rm` create and remove directories before the sandbox and then exit.
- Do not grant `EXECUTE` anywhere.
- The TTY fd is opened before restriction, so `IOCTL_DEV` does not affect it. Do not grant `IOCTL_DEV`.

### 5.3 Enforcement

- Flags: `TSYNC` (the crate's `all_threads`). Crystal is single-threaded, so this is defense in depth only.
- no_new_privs is already set by section 4; Landlock requires it and needs nothing more (the crate sets it once more, harmlessly).
- Do not set `LOG_SAME_EXEC_OFF`. Denials must reach the audit log.
- After `restrict_self`, Crystal checks the crate's status: fully enforced on all threads, or it refuses to run, so a crate bug cannot leave it unconfined.
- *As built (step S3, `src/harden/landlock.rs`):* the rules above, from the vault and state fds; called in `crystal::run` (src/lib.rs) right after the vault directory is opened, before the passphrase prompt ([decision 0059](../decisions.md#0059)). A mutation check (no `MAKE_REG`) made every saving pty test fail, so the ruleset is enforced in the test suite.

### 5.4 Fallback policy

| Running ABI | Behavior |
|-------------|----------|
| >= 9 | Full policy as above |
| < 9, unavailable or disabled | Refuse to start, naming the required ABI (the crate does not report the running one) |

There is no weaker mode and no flag to allow one: Crystal never runs with a partial sandbox. ABI 9 is required because `RESOLVE_UNIX` arrives there; the scopes need ABI 6. `SandboxReport` records the ABI.

## 6. seccomp-bpf

Module: `src/harden/seccomp.rs`. Crate: `seccompiler`.

- Mode: allowlist, default action `KillProcess` in every build. There is no log build and no feature that weakens the action, so none can ship.
- The allowlist is built from what Crystal really calls: the full debug and release test suites (unlock, view, search, edit, save, lock, re-unlock, restore, suspend, resize) run with the log action as a local, uncommitted change, and `run0 ausearch -m SECCOMP -c crystal -i` reads the audit log (auditd keeps it root-only; a positive control, one needed syscall left out, shows the log works). Then the action goes back to kill and the suites run again. Every allowed syscall gets a row in [Syscalls](../reference/syscalls.md). `strace` is not used locally (secureblue's `deny_ptrace` stays on).
- MUST deny (the allowlist omits them; tests cover each): `socket`, `socketpair`, `connect`, `bind`, `execve`, `execveat`, `ptrace`, `process_vm_readv`, `process_vm_writev`, `bpf`, `io_uring_setup`, `io_uring_enter`, `io_uring_register`, `userfaultfd`, `perf_event_open`, `keyctl`, `add_key`, `mount`, `unshare`, `setns`, `personality`, `memfd_create`, `clone`, `clone3`.
- `ioctl`: allow only by request (the low 32 bits, which the kernel reads): `TCGETS2`, `TCSETS2`, `TCSETSF2` (the termios requests rustix actually uses; its fallback to `TCGETS` happens only on an error return, which a seccomp kill never gives), `TIOCGWINSZ` and `TCFLSH` (every prompt discards keys typed before it appeared, inside the sandbox since [0059](../decisions.md#0059)). `TIOCSTI` and `TIOCLINUX` kill (defense in depth, even though modern kernels require `CAP_SYS_ADMIN` for them).
- `kill`: own pid only and `SIGSTOP` only (suspend on `SIGTSTP`; Ctrl+Z is a key since [decision 0045](../decisions.md#0045): Crystal restores the terminal, then stops itself). `getpid` is allowed for the same reason.
- `fcntl`: only `F_DUPFD_CLOEXEC` (a second terminal fd on resume, std's `File::try_clone`) and `F_SETFD` with exactly `FD_CLOEXEC` (musl's `fcntl()` sets it once more after the duplicate, for old kernels, and ignores the result); debug builds also allow `F_GETFD` (std's `debug_assert_fd_is_open` on every close), release builds never.
- `prlimit64`: only reading `RLIMIT_MEMLOCK` of this process (`crypto::scrypt_policy`); the new-limit pointer must be NULL, so no limit can be raised or lowered.
- `mmap`: never with `PROT_EXEC`; `mprotect` is not allowed at all, so no memory can become executable.
- *As built (step S5):* the rules above, in `src/harden/seccomp.rs`; 23 syscalls in all ([Syscalls, The allowlist](../reference/syscalls.md#the-allowlist)). Mutation-checked: without `TCFLSH` every prompt dies, and `kill` limited to another signal kills the suspend.
- `clone` and `clone3`: denied. Crystal is single-threaded.
- Apply with `apply_filter_all_threads` even though no threads exist.

## 7. SELinux policy module

Directory: `packaging/selinux/` containing `crystal.te`, `crystal.fc`, `crystal.if` ([decision 0061](../decisions.md#0061)). A layer Crystal cannot switch off itself: it holds even if Crystal's own Landlock and seccomp code had a bug, and it confines the process from `exec` on, before Crystal's own sandbox exists. It also stops what only seccomp stops otherwise (section 9: `socketpair`, threads, `memfd_create`) from reaching anything: no network class, no other file, no IPC is allowed.

### 7.1 Types

- `crystal_t`: process domain (`userdom_user_application_domain`).
- `crystal_exec_t`: label for `~/.local/bin/crystal`, a per-user install ([Installation](../guide/installation.md)). GNU `install` labels a new file from the loaded policy, so an install after the module is loaded gets `crystal_exec_t`; one made before keeps `home_bin_t`. `home_bin_t` is a *customizable type*, which a plain `restorecon` never changes, so every install MUST be followed by `restorecon -F ~/.local/bin/crystal`, before the optional `chattr +i` (an immutable file cannot be relabeled); otherwise Crystal silently runs outside `crystal_t` ([Setup, Installing](../guide/setup.md#installing)). The label is not an integrity check: a same-user process that can relabel files can also label a fake binary `crystal_exec_t`. That fake then runs confined in `crystal_t`, which limits it but does not stop it asking for the passphrase. Integrity comes from the optional immutable flag.
- `crystal_vault_t`: label for `~/.local/share/crystal` and everything under it.
- `crystal_state_t`: label for `~/.local/state/crystal`.
- No config type: Crystal has no config file ([decision 0032](../decisions.md#0032)). One is added with the decision that adds settings.

### 7.2 Policy (as built)

Every rule was checked against what the code does; `audit2allow` output was never pasted in.

- Domain transition `unconfined_t` -> `crystal_t` on exec of `crystal_exec_t` (`unconfined_run_to`), with use of the user's terminals (`/dev/tty`, `user_devpts_t`) and the shell's inherited fds.
- The process: `setrlimit` (core and memlock limits), `sigstop` and `signal` to itself (the suspend), its own signal pipes, and listing its own `/proc/self/fd` (the inherited-fd check before the sandbox).
- The walk from `HOME`: `search` and `getattr` only on `user_home_dir_t`, `gconf_home_t` (`~/.local`, `~/.local/state`) and `data_home_t` (`~/.local/share`). Crystal opens these with `O_PATH` and never lists them, so no `read` is granted: `crystal_t` cannot list anything outside its two trees.
- Manage dirs and files of `crystal_vault_t` and `crystal_state_t` (reads, atomic writes, `crystal init` and `crystal rm`).
- Creating `~/.local/share/crystal` and `~/.local/state/crystal`: `write` and `add_name` on the parent plus a named `type_transition` to `crystal_vault_t` or `crystal_state_t`. Creating anything else there needs `create` on another type, which `crystal_t` lacks; there is no `read` or `remove_name`, so nothing there can be listed, renamed or removed. `filetrans_pattern` is not used: it grants full read and write on the parent. Missing parents (`~/.local`, `share`, `state`) are not created under this policy.
- Deny by omission: all network classes, `execute` on anything (no plugin until Phase 8), every other file in the home directory, D-Bus, X11/Wayland sockets, `ptrace`, `userns_create`, `execmem`. No `unconfined_domain()` and no broad `*_home_t` macros.
- Phase 8 only: executing the age plugin for hardware-key unlock, as narrowly as possible (one plugin type, no transition to anything broader). Crystal sets `PR_SET_NO_NEW_PRIVS` in startup step 2, so a transition to a plugin domain needs `nnp_transition` or a bounded plugin domain.

### 7.3 File contexts (`crystal.fc`)

```
HOME_DIR/\.local/bin/crystal              --  gen_context(system_u:object_r:crystal_exec_t,s0)
HOME_DIR/\.local/share/crystal(/.*)?          gen_context(system_u:object_r:crystal_vault_t,s0)
HOME_DIR/\.local/state/crystal(/.*)?          gen_context(system_u:object_r:crystal_state_t,s0)
```

Vaults exist only in `~/.local/share/crystal`, so the static rule covers every vault; no per-vault `semanage` rule is ever needed.

### 7.4 Building, installing, checking

Building needs no root (`selinux-policy-devel`; build in a scratch directory, since the Makefile writes a `tmp/` beside the sources):

```sh
make -f /usr/share/selinux/devel/Makefile crystal.pp
```

Installing and checking need root (`run0`, or a root shell):

```sh
semodule -i crystal.pp
restorecon -RFv ~/.local/bin/crystal ~/.local/share/crystal ~/.local/state/crystal
semanage permissive -a crystal_t     # while developing only
# exercise a full session, then list each distinct denial once:
ausearch -m AVC,USER_AVC -c crystal -i -ts <start time>
semanage permissive -d crystal_t     # MUST be removed again
```

Rules:
- Every denial is explained from the code before a rule is added; prefer changing the code so the permission is not needed (as with `O_PATH` above). Never apply `audit2allow` output blindly.
- In permissive mode each distinct denial is logged once; loading the module flushes the cache, so a fresh session after `semodule -i` shows everything.
- Verify: `ps -eZ | grep crystal` shows `crystal_t`.
- *As checked (step S9, 2026-11-16, Fedora 45 policy 45.24):* a full session through a pty (`init`, `unlock`, edit and save, `view`, `search`, `lock` and unlock again, an outside `SIGTSTP` and resume, `check-code`, `rm`) ran in `crystal_t`, permissive first: the only denial, `read` on the home directory, was removed by opening the walk with `O_PATH`, after which the same session logged none, and then passed in enforcing mode.
- The bubblewrap launcher (section 8, step S10) runs Crystal under bwrap's domain and with no_new_privs set by bwrap; it needs its own check that the transition to `crystal_t` still happens (`nnp_transition`).

## 8. Bubblewrap launcher (optional)

File: `packaging/crystal-sandboxed.sh`. Works on secureblue through its patched, SELinux-confined bwrap. Crystal is a static binary, so only the binary itself is bound, not `/usr` or any libraries.

```sh
#!/bin/sh
# Usage: crystal-sandboxed.sh VAULT_NAME [crystal options]
set -eu
name=$1; shift
case $name in                                       # same rule as Crystal's; checked here
  ''|[!A-Za-z0-9]*|*[!A-Za-z0-9_-]*) echo "invalid vault name" >&2; exit 1 ;;
esac                                                # too, so a bad name binds nothing
vault=$HOME/.local/share/crystal/$name
state=$HOME/.local/state/crystal
bin=$HOME/.local/bin/crystal
mkdir -p -m 700 "$state"                            # must exist to be bound
exec bwrap \
  --unshare-all --die-with-parent \
  --clearenv \
  --setenv HOME "$HOME" --setenv TERM "$TERM" \
  --ro-bind "$bin" "$bin" \
  --proc /proc --dev /dev --tmpfs /tmp \
  --ro-bind-try /etc/localtime /etc/localtime \
  --perms 0700 --dir "$HOME/.local/share/crystal" \
  --bind "$vault" "$vault" \
  --bind "$state" "$state" \
  "$bin" unlock "$@" "$name"
```

- `--clearenv` plus `--setenv` mirrors Crystal's own environment allowlist (`HOME`, `TERM`).
- Only the one vault is bound; `--perms 0700 --dir` recreates an empty `~/.local/share/crystal` so Crystal's owner and mode check on it passes. The other vaults do not exist inside the sandbox.
- The launcher opens existing vaults only; run `crystal init`, `crystal recover`, `crystal check-code` and `crystal rm` directly.
- Settings in `~/.config/crystal`, if a later decision puts them there, will be bound read-only.
- Do not bind D-Bus, Wayland, X11 or PulseAudio/PipeWire sockets.
- `--new-session` is not used: it breaks TUI job control, and `TIOCSTI` is blocked by seccomp (section 6).
- `RLIMIT_MEMLOCK` is inherited, so the [Setup](../guide/setup.md) limit still applies.

## 9. Verification (acceptance criteria)

Sandbox enforcement is irreversible, so tests MUST run the binary as a subprocess.

The hidden option `crystal unlock --sandbox-check-for-test CHECK NAME` is compiled into debug builds only, like the other `*-for-test` options (release builds refuse it; `cli::tests::release_refuses_the_test_hooks`). Crystal enters the sandbox exactly as before a real unlock, attempts the one forbidden action `CHECK` names, and prints one line, `sandbox-check: denied errno=N` or `sandbox-check: ALLOWED`. One check per process, because a seccomp kill ends the process; for those checks the test asserts death by `SIGSYS` instead of a line. No JSON: that would need a new dependency for no gain. `tests/sandbox.rs` asserts on the result; its vault is a directory holding only `VERSION`, so no passphrase or scrypt is needed. The checks open files with rustix's `openat`, as Crystal does everywhere: std's `File::open` makes musl call the old `open` syscall, which seccomp kills before Landlock could answer (so any `std::fs` call added later dies in the pty tests).

Status checks are made by the test process from outside, through `/proc/<pid>/status` while the child waits, because Landlock denies `/proc` inside the sandbox.

- [x] Reading `$HOME/.ssh/id_ed25519` (a fixture in the test's temporary HOME) fails with `EACCES` (`reading_outside_the_vault_is_denied`)
- [x] Writing outside the vault and state dirs fails with `EACCES`, and nothing appears (`writing_outside_the_vault_is_denied`)
- [x] Another vault in the same `~/.local/share/crystal` is unreachable (`another_vault_is_unreachable`). Mutation-checked: without Landlock all three report `ALLOWED`
- [x] `socket(AF_INET, ...)` is killed (`seccomp_kills_forbidden_syscalls`), and denied by Landlock alone (`landlock_alone_stops_network_signals_and_exec`, `EACCES`)
- [x] Connecting to a pathname UNIX socket (a live fixture socket the test itself can connect to) fails under Landlock alone with `EACCES`; with seccomp, `socket` dies first
- [x] `execve("/usr/bin/true")` is killed, and denied by Landlock alone (`EACCES`)
- [x] Signalling the parent (the harmless `SIGCONT`, so a failing check hurts nothing) is killed, and denied by Landlock alone (`EPERM`, the signal scope)
- [x] An `ioctl` outside the five is killed (`FIONBIO` on the terminal); `TIOCSTI` and `TIOCLINUX` have no safe wrapper, so a unit test checks that neither is in the list (`tiocsti_and_tioclinux_are_not_allowed`)
- [x] Also killed: `socketpair`, a thread, `memfd_create`, `mkdirat` in the vault (`mkdirat` is also denied by Landlock alone)

The `landlock-*` checks apply Landlock without seccomp (`harden::enter_landlock_only_for_test`, debug builds only), to show it holds on its own. Mutation-checked per check: under Landlock alone none of the eight seccomp checks dies, and without any sandbox none of the five Landlock checks is refused as expected (TCP then gives `ECONNREFUSED`, and `exec` really runs `/usr/bin/true`). Under Landlock alone, `socketpair`, a thread, `memfd_create` and the `FIONBIO` ioctl were allowed: for these seccomp is the only layer until the SELinux module (section 7).
- [x] `/proc/<pid>/status` shows `NoNewPrivs: 1`, `Seccomp: 2`, `Seccomp_filters: 1` (`running_sandbox_shows_in_proc_status`)
- [x] `cat /proc/<crystal pid>/mem` from another shell fails (same test)
- [x] Save and atomic rename still work inside the sandbox: every saving test in `tests/vault.rs` runs inside it
- [x] `ps -eZ` shows `crystal_t` (manual check with the module installed, step S9): a full session (`init`, `unlock`, edit and save, `view`, `search`, `lock` and unlock, outside suspend and resume, `check-code`, `rm`) runs in `crystal_t` in enforcing mode
- [x] Without Landlock, Crystal refuses to start before any prompt and names ABI 9 (`refuses_to_start_without_landlock`): the debug-only `--hide-landlock-for-test` installs a one-rule seccomp filter so the crate's ABI query gets `ENOSYS`, as from a kernel built without Landlock, and the real refusal path runs. A kernel with ABI 1 to 8 cannot be faked this way (seccomp returns errors, not a version); it fails the same hard-requirement step in the crate. Mutation-checked: in best-effort mode the status check after enforcing still refuses ("not fully enforced"); without that check too, Crystal reached the passphrase prompt unconfined and the test failed
- [ ] CI runs the suite on a kernel with ABI >= 9. GitHub's standard runner kernel may be older, and Crystal will not start there; Phase 6 must provide such a kernel (newer runner image or a VM) rather than a weaker mode

## 10. References

- Landlock userspace API: https://docs.kernel.org/userspace-api/landlock.html
- Kernel UAPI header: `/usr/include/linux/landlock.h`
- `landlock` crate: https://docs.rs/landlock
- `seccompiler`: https://docs.rs/seccompiler
- Bubblewrap: https://github.com/containers/bubblewrap
- secureblue user namespaces: https://secureblue.dev/articles/userns
- secureblue Trivalent SELinux policy (reference for 7): `files/scripts/selinux/trivalent` in the secureblue repo
