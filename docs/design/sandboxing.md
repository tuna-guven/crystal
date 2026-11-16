# Crystal Sandboxing Design

Status: Draft
Scope: Process confinement for the `crystal` binary (offline, age-encrypted Markdown notes TUI)
Target platform: secureblue (Fedora Atomic, SELinux enforcing). Verified 2026-10-12 on kernel 7.2.9: Landlock ABI 10. The `landlock` crate (0.4.7) supports up to ABI 9, which is what Crystal uses (section 5.1).

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
- All sandbox code lives in the `harden` module (`src/harden/landlock.rs`, `src/harden/seccomp.rs`) and is called from one function: `harden::enter_sandbox(vault, state) -> Result<(), HardenError>`, given the vault and state directory fds of step 3. *As built (step S3):* Landlock only; seccomp joins in step S5.

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
- *As built (step S3, `src/harden/landlock.rs`):* the rules above, from the vault and state fds; called in `main` right after the vault directory is opened, before the passphrase prompt ([decision 0059](../decisions.md#0059)). A mutation check (no `MAKE_REG`) made every saving pty test fail, so the ruleset is enforced in the test suite.

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

Directory: `packaging/selinux/` containing `crystal.te`, `crystal.fc`, `crystal.if`.

### 7.1 Types

- `crystal_t`: process domain.
- `crystal_exec_t`: label for `~/.local/bin/crystal`, a per-user install ([Installation](../guide/installation.md)). Installing a new build creates a new file with the default label, so every install MUST be followed by `restorecon ~/.local/bin/crystal`, before the optional `chattr +i` (an immutable file cannot be relabeled); otherwise Crystal silently runs outside `crystal_t` ([Setup, Installing](../guide/setup.md#installing)). The label is not an integrity check: a same-user process that can relabel files can also label a fake binary `crystal_exec_t`. That fake then runs confined in `crystal_t`, which limits it but does not stop it asking for the passphrase. Integrity comes from the optional immutable flag.
- `crystal_vault_t`: label for `~/.local/share/crystal` and everything under it.
- `crystal_state_t`: label for `~/.local/state/crystal`.
- `crystal_config_t`: label for `~/.config/crystal` (plain-text settings, if a later decision puts settings there). `crystal_t` reads it before the sandbox and never writes it.

### 7.2 Policy intent

- Domain transition `unconfined_t` -> `crystal_t` on exec of `crystal_exec_t`.
- Allow: manage files and dirs of `crystal_vault_t` and `crystal_state_t`; create `crystal_vault_t` dirs under `~/.local/share` and `crystal_state_t` dirs under `~/.local/state`; read `crystal_config_t`; read/write the user TTY/PTY (`user_tty_device_t`, `user_devpts_t`); read locale and timezone files.
- Phase 8 only: execute the age plugin binary for hardware-key unlock, as narrowly as possible (one plugin type, no transition to anything broader). Crystal sets `PR_SET_NO_NEW_PRIVS` in startup step 2, before the plugin runs, so a transition from `crystal_t` to a plugin domain needs `allow crystal_t <plugin>_t:process2 nnp_transition;` or a bounded plugin domain. Otherwise run the plugin in `crystal_t` with no transition.
- Deny by omission: all network classes, `execute` on anything else, access to other `user_home_t` content, D-Bus, X11/Wayland sockets, `ptrace`, `userns_create`.
- Do not use `unconfined_domain()` or broad `*_home_t` manage macros.

### 7.3 File contexts (`crystal.fc`)

```
HOME_DIR/\.local/bin/crystal              --  gen_context(system_u:object_r:crystal_exec_t,s0)
HOME_DIR/\.config/crystal(/.*)?               gen_context(system_u:object_r:crystal_config_t,s0)
HOME_DIR/\.local/share/crystal(/.*)?          gen_context(system_u:object_r:crystal_vault_t,s0)
HOME_DIR/\.local/state/crystal(/.*)?          gen_context(system_u:object_r:crystal_state_t,s0)
```

Vaults exist only in `~/.local/share/crystal`, so the static rule covers every vault; no per-vault `semanage` rule is ever needed.

### 7.4 Development workflow

```sh
sepolicy generate --application ~/.local/bin/crystal -n crystal    # starting template only
make -f /usr/share/selinux/devel/Makefile crystal.pp
run0 semodule -i crystal.pp
run0 restorecon -Rv ~/.local/bin/crystal ~/.config/crystal ~/.local/state/crystal ~/.local/share/crystal
run0 semanage permissive -a crystal_t           # domain-level permissive while developing
# exercise a full session, then:
run0 ausearch -m AVC -c crystal | audit2allow -R
run0 semanage permissive -d crystal_t           # MUST remove before release
```

Rules:
- Every `audit2allow` suggestion is reviewed by hand. Never apply its output blindly.
- Verify: `ps -eZ | grep crystal` shows `crystal_t`.
- Test both launch paths: direct and via the bwrap launcher. Launching through bwrap runs Crystal under bwrap's domain context; confirm the transition to `crystal_t` happens or add the needed transition rule.

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
- [ ] `socket(AF_INET, ...)` is killed or denied
- [ ] Connecting to a pathname UNIX socket (a fixture socket in the test directory) fails
- [ ] `execve("/usr/bin/true")` is killed or denied
- [ ] `kill(<parent pid>, SIGTERM)` fails (Landlock signal scope)
- [ ] `ioctl(tty, TIOCSTI, ...)` is killed or denied
- [x] `/proc/<pid>/status` shows `NoNewPrivs: 1`, `Seccomp: 2`, `Seccomp_filters: 1` (`running_sandbox_shows_in_proc_status`)
- [x] `cat /proc/<crystal pid>/mem` from another shell fails (same test)
- [x] Save and atomic rename still work inside the sandbox: every saving test in `tests/vault.rs` runs inside it
- [ ] `ps -eZ` shows `crystal_t` (when policy installed; manual check)
- [ ] A kernel below ABI 9 makes Crystal refuse to start (test on a kernel or VM without it, or with Landlock disabled)
- [ ] CI runs the suite on a kernel with ABI >= 9. GitHub's standard runner kernel may be older, and Crystal will not start there; Phase 6 must provide such a kernel (newer runner image or a VM) rather than a weaker mode

## 10. References

- Landlock userspace API: https://docs.kernel.org/userspace-api/landlock.html
- Kernel UAPI header: `/usr/include/linux/landlock.h`
- `landlock` crate: https://docs.rs/landlock
- `seccompiler`: https://docs.rs/seccompiler
- Bubblewrap: https://github.com/containers/bubblewrap
- secureblue user namespaces: https://secureblue.dev/articles/userns
- secureblue Trivalent SELinux policy (reference for 7): `files/scripts/selinux/trivalent` in the secureblue repo
