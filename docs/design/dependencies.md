# Dependencies

About a dozen direct dependencies, all pure Rust, no async runtime, no C libraries. Pin exact versions in `Cargo.lock` and check each crate's compatibility with the ratatui version you choose.

| Need | Crate | Notes |
| --- | --- | --- |
| TUI layout and widgets | `ratatui` | Immediate-mode rendering; use only the widgets you need |
| Terminal backend | `crossterm` | Raw mode, alternate screen, events, bracketed paste |
| Editor widget | `tui-textarea` | Start here; replace with your own on `ropey` if its copying worries you |
| Markdown parser | `pulldown-cmark` | `default-features = false`, no HTML feature; you write the renderer |
| Encryption | `age` | The library crate of the rage project; X25519 and scrypt recipients; enable the plugin feature only for YubiKey or TPM unlock |
| Secret types | `secrecy`, `zeroize` | `SecretString` to hand passphrases and keys to age; `Zeroizing<Vec<u8>>` and derive `ZeroizeOnDrop` for plaintext. The vault identity is a `Box`, not a `SecretBox` ([decision 0013](../decisions/0013-identity-box.md)) |
| Hashing | `sha2` | Already in age's tree; used for backup snapshot hashes |
| Serialization | `serde` + `postcard` | Compact, schema-light, small attack surface for the vault contents |
| Syscalls | `rustix` | prctl, setrlimit, mlockall, fsync, renameat without writing `unsafe` yourself |
| Filesystem sandbox | `landlock` | Restrict file access to the vault and state directories. Version 0.4.7 supports Landlock ABI up to 9; no raw Landlock syscalls, which would need `unsafe` ([Sandboxing](sandboxing.md) 5.1) |
| Syscall filter | `seccompiler` | Pure Rust BPF builder from Firecracker; avoids the C libseccomp |
| CLI args | `lexopt` | Tiny; you only parse a vault path and a few flags |

**About rage:** rage is the Rust implementation of age, and its library ships as the `age` crate in the rage repository. Link that crate directly; never shell out to the `rage` binary, which would need `execve` and push plaintext through pipes.

**Avoid:** `tokio` or any async runtime, HTTP clients, `clap` (large for this job), syntax highlighters (`syntect`), `tui-markdown` or other ready-made renderers, image crates, file watchers, logging crates that write files, `rpassword` (read the passphrase yourself in raw mode).

**Release profile:** `lto = true`, `codegen-units = 1`, `strip = true`, `overflow-checks = true`, keep `panic = "unwind"` so `Drop` zeroizes on panic. Target `x86_64-unknown-linux-musl` for a static binary. Put `#![forbid(unsafe_code)]` on the crate. The zeroizing allocator, the only `unsafe` code, lives in a separate workspace crate `crates/zalloc`; the `sys` module only installs it.

# Build vs reuse

Reuse anything that is hard to get right and well reviewed (crypto, parsing, terminal I/O); build everything that defines Crystal's security properties.

| Component | Decision | Size estimate | Why |
| --- | --- | --- | --- |
| Encryption primitives | Reuse `age` | n/a | Never roll your own crypto |
| Markdown parsing | Reuse `pulldown-cmark` | n/a | Parsers are where bugs live |
| Terminal I/O and layout | Reuse `crossterm` + `ratatui` | n/a | Mature, widely used |
| Sandbox plumbing | Reuse `landlock` + `seccompiler` | n/a | Kernel ABI details are easy to get wrong |
| Vault format | Build | 600 lines | Defines integrity, rollback and metadata hiding |
| Atomic writer | Build | 80 lines | Ciphertext-only, temp + fsync + rename + dir fsync |
| Markdown renderer | Build | 300 lines | Small subset, full control over output |
| Sanitizer | Build | 60 lines | Strips C0, C1, ESC, BiDi overrides from every rendered span |
| Wikilink and backlink index | Build | 200 lines | Simple map over titles, rebuilt on unlock |
| Fuzzy finder and search | Build | 150 lines | Subsequence scorer plus case-insensitive substring search |
| Terminal guard | Build | 120 lines | Alt screen, raw mode, clear on exit, panic hook, signals |
| Hardening bootstrap | Build | 150 lines | prctl, rlimits, mlockall, env scrub, tty checks |
| Passphrase prompt | Build | 60 lines | Reads from /dev/tty in raw mode into zeroizing memory |
| Zeroizing allocator | Build | 40 lines | GlobalAlloc wrapper that zeroes on free and on realloc |
| Editor | Reuse first, maybe build later | 800+ lines if built | `tui-textarea` is fine for v1 |

The zeroizing allocator is the key trick for Rust. `String` and `Vec` growth reallocates and leaves old copies in freed heap where `zeroize` cannot reach them. A global allocator that zeroes every block before freeing it closes that gap for the whole program, including inside `tui-textarea` and `pulldown-cmark`. The cost is negligible for a notes app.
