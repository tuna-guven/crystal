# Dependencies

As few direct dependencies as possible (Crystal is meant to be minimal and auditable), all pure Rust, no async runtime, no C libraries. Crates that handle secrets or untrusted input are pinned exactly in `Cargo.toml`.

| Need | Crate | Notes |
| --- | --- | --- |
| Screen grid and widgets | Crystal's own (`ui::grid`, `ui::screen`) | No ratatui: Crystal used a fifth of it and got 28 crates ([decision 0031](../decisions/0031-own-grid.md)) |
| Display width | `unicode-width` =0.2.3 (no default features, so no `cjk` tables) | Columns a grapheme takes; no dependencies, `forbid(unsafe_code)` (decision 0031) |
| Grapheme clusters | `unicode-segmentation` =1.13.3 | A character with its combining marks, or an emoji sequence, fills one cell; no dependencies (decision 0031) |
| Terminal backend | Crystal's own (`term`) | Raw mode, alternate screen and key reading already live in `term::TerminalGuard`; the frame is drawn to `/dev/tty` by Crystal, not `crossterm` ([decision 0030](../decisions/0030-viewer-dependencies.md)) |
| Editor widget | Crystal's own | Phase 4, on the grid; `tui-textarea` needs the full `ratatui` and `crossterm` (decision 0031) |
| Markdown parser | `pulldown-cmark` =0.13.6 | `default-features = false`: no HTML writer, no `getopts`; Crystal writes the renderer |
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
| Unicode width and graphemes | Reuse `unicode-width` + `unicode-segmentation` | n/a | Unicode tables change with each release |
| Grid, wrapping and widgets | Build | 600 lines | Crystal uses a grid, a diff and four widgets; ratatui would bring 28 crates (decision 0031) |
| Terminal output | Build | 150 lines | Crystal already owns the tty; every byte to the terminal passes through its code (decisions 0030, 0031) |
| Sandbox plumbing | Reuse `landlock` + `seccompiler` | n/a | Kernel ABI details are easy to get wrong |
| Vault format | Build | 600 lines | Defines integrity, rollback and metadata hiding |
| Atomic writer | Build | 80 lines | Ciphertext-only, temp + fsync + rename + dir fsync |
| Markdown renderer | Build | 300 lines | Small subset, full control over output |
| Sanitizer | Build | 60 lines | Strips C0, C1, ESC, BiDi overrides from every rendered span |
| Wikilink and backlink index | Build | 200 lines | Simple map over titles, rebuilt on unlock |
| `find` and `search` | Build | 150 lines | Subsequence scorer over names, case-insensitive full-text search ([decision 0033](../decisions/0033-shell.md)) |
| Terminal guard | Build | 120 lines | Alt screen, raw mode, clear on exit, panic hook, signals |
| Hardening bootstrap | Build | 150 lines | prctl, rlimits, mlockall, env scrub, tty checks |
| Passphrase prompt | Build | 60 lines | Reads from /dev/tty in raw mode into zeroizing memory |
| Zeroizing allocator | Build | 40 lines | GlobalAlloc wrapper that zeroes on free and on realloc |
| Editor | Build | 800 lines | On the grid, in zeroizing memory; `tui-textarea` needs ratatui and crossterm (decision 0031) |

The zeroizing allocator is the key trick for Rust. `String` and `Vec` growth reallocates and leaves old copies in freed heap where `zeroize` cannot reach them. A global allocator that zeroes every block before freeing it closes that gap for the whole program, including inside `pulldown-cmark`. The cost is negligible for a notes app.
