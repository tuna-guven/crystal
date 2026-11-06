# Testing and verification

Each security property gets a test that would fail if the property broke; tests run in CI on every commit.

| Test | What it proves | Tooling |
| --- | --- | --- |
| Round-trip properties | Encrypt then decrypt returns the same note; padding is reversible | `proptest` |
| Tamper suite | A vault with another vault\_id, truncation, bit flips, an older generation: all refused | Integration tests over a temp vault |
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
