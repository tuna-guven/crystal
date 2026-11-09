# Testing and verification

Integration tests drive the real binary in a pseudo-terminal (`script`); their shared helpers are in `tests/common/mod.rs`. Tests of the shell and pager open a vault and add sample notes in memory with the debug-only flag `--demo-notes-for-test` (not in release builds, so those tests are ignored there; nothing is saved). Tests of startup steps 1 to 7, signals and the terminal guard pass the debug-only `--skip-unlock-for-test`, so they need no vault; release builds have no such flag. Under `cargo test --release`, the tests that need a debug-only hook are reported as ignored; everything else, including the tamper suite, runs against the release binary. Run it at the 512 MiB memlock floor too (`prlimit --memlock=536870912:536870912 cargo test --release`): only that run shows that the release binary's scrypt (log N 18, the same as every build; [decision 0027](../decisions/0027-scrypt-fixed.md)) fits in locked memory with everything else. Debug and test builds optimize the scrypt crates (`[profile.dev.package.*]` in Cargo.toml) so the integration tests stay fast.

Each security property gets a test that would fail if the property broke; tests run in CI on every commit.

| Test | What it proves | Tooling |
| --- | --- | --- |
| Property tests | Round trips (codec, padding, encryption); the renderer, sanitizer, wrapping, names and key decoding never panic, never put a control or BiDi character on screen and never draw a line wider than the page | `proptest`: 256 cases per test on every `cargo test`; `scripts/long-tests.sh` runs a million per test for the text-handling modules (about 7 minutes), weekly in CI (`.github/workflows/long-tests.yml`) and before releases |
| Tamper suite | A vault swapped in under another's name (pin), truncation, bit flips: all refused; an older generation opens only after the word `restore`, and anything else changes nothing | `tests/vault.rs`: each test makes a vault with `crystal init` through the real binary in a pty, damages one thing (vault or config truncated, bit-flipped, rolled back or swapped, a wrong passphrase, a missing `identity.age`, a writable parent, a sync conflict copy) and checks the fixed refusal |
| Coverage-guided fuzzing | No panics or hangs on hostile Markdown or vault bytes, beyond what random generation reaches | `cargo-fuzz` targets for render, sanitize and vault decode in a separate `fuzz/` crate; from Phase 6 (needs `libfuzzer-sys` and a nightly toolchain, to be approved then) |
| Escape-injection corpus | OSC 52, OSC 8, title changes, CSI, DCS, APC, 8-bit C1, BiDi, NUL, zero-width and overlong lines, in every Markdown context, render inert | `tests/corpus/escape/`: `md::render::tests::escape_corpus` checks every cell and compares with golden files (`UPDATE_GOLDEN=1` to accept a deliberate change) |
| Disk leak test | Only `*.age` and ciphertext temp files are ever created | `strace -f`, `inotifywait -r` on home and /tmp during a scripted session |
| Canary scan | After lock, a unique canary string from a note no longer exists in process memory | Test-only feature that reads `/proc/self/mem` before dumpable is cleared |
| Hardening checks | Non-dumpable, no core, memory locked, no new privs | Integration test reading `/proc/self/status` |
| Sandbox checks | Denied syscalls kill the process; paths outside the vault return EACCES | Child-process tests through the debug-only `--sandbox-selftest CHECK`, one check per process ([Sandboxing](../design/sandboxing.md) section 9) |
| Unsafe code | The allocator is sound | `cargo miri test` on `zalloc` |
| Dependency audit | No known vulnerabilities, licenses allowed, crates reviewed | `cargo audit`, `cargo deny`, `cargo vet`, `cargo geiger` |
| Reproducibility | Same source gives the same binary | Two clean builds, compare SHA-256 |

Because this is a security tool, plan for an outside review once Phase 6 is done: a classmate, a security-focused community review, or a small paid audit before you rely on it for anything sensitive.
