# Testing and verification

Integration tests drive the real binary in a pseudo-terminal (`script`); their shared helpers are in `tests/common/mod.rs`. Tests of startup steps 1 to 7, signals and the terminal guard pass the debug-only `--skip-unlock-for-test`, so they need no vault; release builds have no such flag. Under `cargo test --release`, the tests that need a debug-only hook are reported as ignored; everything else, including the tamper suite, runs against the release binary. Run it at the 512 MiB memlock floor too (`prlimit --memlock=536870912:536870912 cargo test --release`): only that run shows that the release binary's scrypt (log N 18, the same as every build; [decision 0027](../decisions/0027-scrypt-fixed.md)) fits in locked memory with everything else. Debug and test builds optimize the scrypt crates (`[profile.dev.package.*]` in Cargo.toml) so the integration tests stay fast.

Each security property gets a test that would fail if the property broke; tests run in CI on every commit.

| Test | What it proves | Tooling |
| --- | --- | --- |
| Round-trip properties | Encrypt then decrypt returns the same note; padding is reversible | `proptest` |
| Tamper suite | A vault swapped in under another's name (pin), truncation, bit flips: all refused; an older generation opens only after the word `restore`, and anything else changes nothing | `tests/vault.rs`: each test makes a vault with `crystal init` through the real binary in a pty, damages one thing (vault or config truncated, bit-flipped, rolled back or swapped, a wrong passphrase, a missing `identity.age`, a writable parent, a sync conflict copy) and checks the fixed refusal |
| Parser and renderer fuzzing | No panics or hangs on hostile Markdown; sanitizer output contains no control bytes | `cargo-fuzz` targets for vault decode and render |
| Escape-injection corpus | OSC 52 clipboard writes, OSC 8 links, title changes, cursor moves, BiDi tricks are all inert | Golden-file tests on rendered output |
| Disk leak test | Only `*.age` and ciphertext temp files are ever created | `strace -f`, `inotifywait -r` on home and /tmp during a scripted session |
| Canary scan | After lock, a unique canary string from a note no longer exists in process memory | Test-only feature that reads `/proc/self/mem` before dumpable is cleared |
| Hardening checks | Non-dumpable, no core, memory locked, no new privs | Integration test reading `/proc/self/status` |
| Sandbox checks | Denied syscalls kill the process; paths outside the vault return EACCES | Child-process tests through the debug-only `--sandbox-selftest CHECK`, one check per process ([Sandboxing](../design/sandboxing.md) section 9) |
| Unsafe code | The allocator is sound | `cargo miri test` on `zalloc` |
| Dependency audit | No known vulnerabilities, licenses allowed, crates reviewed | `cargo audit`, `cargo deny`, `cargo vet`, `cargo geiger` |
| Reproducibility | Same source gives the same binary | Two clean builds, compare SHA-256 |

Because this is a security tool, plan for an outside review once Phase 6 is done: a classmate, a security-focused community review, or a small paid audit before you rely on it for anything sensitive.
