# Dependencies

As few direct dependencies as possible (Crystal is meant to be minimal and auditable), all pure Rust, no async runtime, no C libraries. Crates that handle secrets or untrusted input are pinned exactly in `Cargo.toml`.

| Need | Crate | Notes |
| --- | --- | --- |
| Screen grid and widgets | Crystal's own (`ui::grid`, `ui::screen`) | No ratatui: Crystal used a fifth of it and got 28 crates ([decision 0031](../decisions/0031-own-grid.md)) |
| Display width | `unicode-width` =0.2.3 (no default features, so no `cjk` tables) | Columns a grapheme takes; no dependencies, `forbid(unsafe_code)` (decision 0031) |
| Grapheme clusters | `unicode-segmentation` =1.13.3 | A character with its combining marks, or an emoji sequence, fills one cell; no dependencies (decision 0031) |
| Terminal backend | Crystal's own (`term`) | Raw mode, alternate screen and key reading already live in `term::TerminalGuard`; the frame is drawn to `/dev/tty` by Crystal, not `crossterm` ([decision 0031](../decisions/0031-own-grid.md)) |
| Editor widget | Crystal's own | Phase 4, on the grid; `tui-textarea` needs the full `ratatui` and `crossterm` (decision 0031) |
| Markdown parser | `pulldown-cmark` =0.13.6 | `default-features = false`: no HTML writer, no `getopts`; Crystal writes the renderer |
| Encryption format | Crystal's own age v1 code (`src/age_v1/`) | Ported from age 0.12.1 under its MIT license, X25519 and scrypt recipients only, checked against the C2SP/CCTV vectors and the `age` crate ([decision 0042](../decisions/0042-own-age-implementation.md)). Since step C2 the program encrypts only through it; the `age` crate is a test-only dependency until C3 removes it |
| AEAD | `chacha20poly1305` =0.10.1 (no default features, `alloc`) | Stanza key wrapping and STREAM chunks; no `getrandom` feature, randomness comes from rustix (decision 0042) |
| Passphrase KDF | `scrypt` =0.11.0 (no default features) | The scrypt stanza; no `password-hash` (decision 0042) |
| X25519 | `x25519-dalek` =2.0.1 (`static_secrets`, `zeroize`, `precomputed-tables`) | The X25519 stanza; keys built from getrandom bytes, wiped on drop (decision 0042) |
| KDF and MAC | `hkdf` =0.12.4, `hmac` =0.12.1, `sha2` =0.10.9, `subtle` =2.6.1 (no default features) | age's HKDF-SHA-256 keys, the HMAC-SHA-256 header MAC, the constant-time all-zero check (decision 0042) |
| Secret types | `zeroize` =1.9.1 | `Zeroizing<Vec<u8>>` and derive `ZeroizeOnDrop` for plaintext, passphrases and keys. `secrecy` was dropped in step C2: only the `age` crate's API needed `SecretString` ([decision 0042](../decisions/0042-own-age-implementation.md)). The vault identity is a `Box`, not a `SecretBox` ([decision 0013](../decisions/0013-identity-box.md)) |
| Serialization | `serde` + `postcard` | Compact, schema-light, small attack surface for the vault contents |
| Syscalls | `rustix` | prctl, setrlimit, mlockall, fsync, renameat without writing `unsafe` yourself |
| Filesystem sandbox | `landlock` | Restrict file access to the vault and state directories. Version 0.4.7 supports Landlock ABI up to 9; no raw Landlock syscalls, which would need `unsafe` ([Sandboxing](sandboxing.md) 5.1) |
| Syscall filter | `seccompiler` | Pure Rust BPF builder from Firecracker; avoids the C libseccomp |
| CLI args | `lexopt` | Tiny; you only parse a vault path and a few flags |

**About age and rage:** Crystal's files are age files, readable by age, rage or any age implementation. Crystal implements the published format itself, ported from rage's library (the `age` crate), because that crate brought about a hundred crates for two recipient types ([decision 0042](../decisions/0042-own-age-implementation.md)). Never shell out to the `rage` or `age` binary, which would need `execve` and push plaintext through pipes.

**Test-only dependencies:** `proptest` =1.12.0 (property tests); `miniz_oxide` =0.8.9 (no default features, `with-alloc`; brings `adler2`) to inflate the C2SP/CCTV age vectors CCTV ships compressed; `age` =0.11.5 (no default features), rage's library, to cross-check Crystal's age code in both directions and to read the real binary's files in the integration tests, until step C3 ([decision 0042](../decisions/0042-own-age-implementation.md)). None of them is in the binary.

**Avoid:** `tokio` or any async runtime, HTTP clients, `clap` (large for this job), syntax highlighters (`syntect`), `tui-markdown` or other ready-made renderers, image crates, file watchers, logging crates that write files, `rpassword` (read the passphrase yourself in raw mode).

**Release profile:** `lto = true`, `codegen-units = 1`, `strip = true`, `overflow-checks = true`, keep `panic = "unwind"` so `Drop` zeroizes on panic. Target `x86_64-unknown-linux-musl` for a static binary. Put `#![forbid(unsafe_code)]` on the crate. The zeroizing allocator, the only `unsafe` code, lives in a separate workspace crate `crates/zalloc`; the `sys` module only installs it.

# Build vs reuse

Reuse anything that is hard to get right and well reviewed (crypto, parsing, terminal I/O); build everything that defines Crystal's security properties.

| Component | Decision | Size estimate | Why |
| --- | --- | --- | --- |
| Encryption primitives | Reuse RustCrypto and dalek (`chacha20poly1305`, `scrypt`, `x25519-dalek`, `hkdf`, `hmac`, `sha2`) | n/a | Never roll your own crypto |
| age v1 format | Port (age 0.12.1, MIT) | 1,200 lines | The published format, not a construction of our own; two recipient types instead of the `age` crate's hundred crates; CCTV vectors and cross-checks ([decision 0042](../decisions/0042-own-age-implementation.md)) |
| Markdown parsing | Reuse `pulldown-cmark` | n/a | Parsers are where bugs live |
| Unicode width and graphemes | Reuse `unicode-width` + `unicode-segmentation` | n/a | Unicode tables change with each release |
| Grid, wrapping and widgets | Build | 600 lines | Crystal uses a grid, a diff and four widgets; ratatui would bring 28 crates (decision 0031) |
| Terminal output | Build | 150 lines | Crystal already owns the tty; every byte to the terminal passes through its code (decision 0031) |
| Sandbox plumbing | Reuse `landlock` + `seccompiler` | n/a | Kernel ABI details are easy to get wrong |
| Vault format | Build | 600 lines | Defines integrity, rollback and metadata hiding |
| Atomic writer | Build | 80 lines | Ciphertext-only, temp + fsync + rename + dir fsync |
| Markdown renderer | Build | 450 lines | `md::render`: a small subset as styled, wrapped lines over the sanitized text, plain text for `*.txt`; one pass, no recursion, lines kept flat ([decision 0038](../decisions/0038-plain-text-notes.md)) |
| Sanitizer | Build | 80 lines | Spells out C0, C1 (ESC, CSI) and BiDi controls as dimmed `<U+XXXX>` in every rendered span; only its `SafeText` reaches a widget ([decision 0037](../decisions/0037-visible-escapes.md)) |
| Wikilink and backlink index | Build | 200 lines | Simple map over titles, rebuilt on unlock |
| `find` and `search` | Build | 150 lines | Subsequence scorer over names, case-insensitive full-text search ([decision 0033](../decisions/0033-shell.md)) |
| Terminal guard | Build | 120 lines | Alt screen, raw mode, clear on exit, panic hook, signals |
| Hardening bootstrap | Build | 150 lines | prctl, rlimits, mlockall, env scrub, tty checks |
| Passphrase prompt | Build | 60 lines | Reads from /dev/tty in raw mode into zeroizing memory |
| Zeroizing allocator | Build | 40 lines | GlobalAlloc wrapper that zeroes on free and on realloc |
| Editor | Build | 800 lines | On the grid, in zeroizing memory; `tui-textarea` needs ratatui and crossterm (decision 0031) |

The zeroizing allocator is the key trick for Rust. `String` and `Vec` growth reallocates and leaves old copies in freed heap where `zeroize` cannot reach them. A global allocator that zeroes every block before freeing it closes that gap for the whole program, including inside `pulldown-cmark`. The cost is negligible for a notes app.
