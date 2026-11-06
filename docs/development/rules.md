# Project rules and workflow

Crystal is built with Claude Code, one phase at a time. The rules below
live in `CLAUDE.md` at the repository root, which Claude Code reads at the
start of every session; this chapter is the published copy.

```markdown
# Crystal rules

Read the design chapters in docs/design/ (the book: docs/SUMMARY.md) and
the decision log in docs/decisions/ before any change. Work on one phase
at a time.

## Hard rules
- Never add a dependency without asking first; state the crate, version and why.
- No network code, no async runtime, no threads unless approved.
- No `unsafe` outside crates/zalloc. The crystal crate is #![forbid(unsafe_code)]
  in every non-test build. Exception: a `#[cfg(test)]` function that checks
  memory (e.g. zeroization) may use `#[allow(unsafe_code)]` with a SAFETY
  comment; test builds use deny(unsafe_code), so nothing else can.
- Note content only lives in `NoteText` (Zeroizing<String>). Never derive or
  implement Debug, Display or Serialize for it. Never put note text in
  errors, logs, panics or format! strings.
- Never write files except through vault::atomic_write or backup::store, and only
  ciphertext, or the fixed public formats `VERSION` and `*.gen` (`PublicBytes`).
- Never spawn processes, except the age plugin during unlock, before the sandbox.
- Any new syscall needs a line in docs/reference/syscalls.md explaining why.
- Never weaken Landlock, seccomp or hardening to make a test pass.

## Documentation
- Every architectural change updates the book (docs/) in the same commit:
  the affected chapter, plus a new record in docs/decisions/ (listed in
  docs/SUMMARY.md). Architectural means every decision important for
  security, design or ease of use: cryptography, file formats, reproducible
  builds, sandboxing, hardening, dependencies, Linux integration, security
  architecture, threat model. Not every implementation detail.
- Never rewrite an accepted decision record; supersede it with a new one.

## Before finishing a task
- cargo fmt --all --check
- cargo clippy --all-targets -- -D warnings
- cargo test
- cargo deny check
- cargo audit
- mdbook build (when docs/ changed)
- List what changed and which acceptance criteria now pass.
```

## Prompts per phase

One session each, in plan mode:

1. Phase 0: "Scaffold the crate per the roadmap, Phase 0. Set up the release profile, musl target, deny.toml and CI. No features yet."
2. Phase 1: "Implement the startup order steps 1 to 7 and the terminal guard. Add tests for every Phase 1 acceptance criterion."
3. Phase 2: "Implement the vault format exactly as specified in the storage chapter. Write the tamper suite first, then the code."
4. Phase 3 onward: "Implement Phase N per the roadmap. Start by listing the acceptance tests you will write."

After each phase, ask a fresh session to review the diff as an attacker: "Review this diff against the threat model. Find any path where plaintext can reach disk, the terminal outside the alt screen, an error message, or another process."
