# DESIGN.md has moved

The design document was split into chapters of the Crystal book
(`docs/SUMMARY.md`; build with `mdbook build`). Older issues and commits
refer to "DESIGN.md, *section*"; this table maps each section to its
chapter. This file is not part of the book.

| DESIGN.md section | Now in |
| --- | --- |
| Overview and goals, Design principles | `docs/design/overview.md` |
| Threat model, Accepted leaks | `docs/design/threat-model.md` |
| Architecture (diagram) | `docs/design/architecture.md` |
| On-disk layout, Config file, Key design, Vault contents, Integrity, Save sequence, Costs of one blob, Open questions | `docs/design/storage.md` |
| Crate selection, About rage, Avoid, Release profile, Build vs reuse | `docs/design/dependencies.md` |
| Plaintext leakage prevention, Exits that skip cleanup | `docs/design/leakage.md` |
| Process hardening and sandboxing, startup steps 1 to 11, seccomp allowlist | `docs/design/startup.md` |
| Installation | `docs/guide/installation.md` |
| Backups | `docs/design/backups.md` |
| Phased build plan | `docs/development/roadmap.md` |
| Testing and verification | `docs/development/testing.md` |
| Working with Claude Code | Removed from the documentation |

`docs/SANDBOXING.md`, `docs/SETUP.md` and `docs/SYSCALLS.md` moved to
`docs/design/sandboxing.md`, `docs/guide/setup.md` and
`docs/reference/syscalls.md`.
