# Crystal rules

Read docs/DESIGN.md before any change. Work on one phase at a time.

## Hard rules
- Never add a dependency without asking first; state the crate, version and why.
- No network code, no async runtime, no threads unless approved.
- No `unsafe` outside crates/zalloc. The crystal crate is #![forbid(unsafe_code)].
- Note content only lives in `NoteText` (Zeroizing<String>). Never derive or
  implement Debug, Display or Serialize for it. Never put note text in
  errors, logs, panics or format! strings.
- Never write files except through vault::atomic_write or backup::store, and only ciphertext.
- Never spawn processes, except the age plugin during unlock, before the sandbox.
- Any new syscall needs a line in docs/SYSCALLS.md explaining why.
- Never weaken Landlock, seccomp or hardening to make a test pass.

## Before finishing a task
- cargo fmt --all --check
- cargo clippy --all-targets -- -D warnings
- cargo test
- cargo deny check
- cargo audit
- List what changed and which acceptance criteria now pass.
