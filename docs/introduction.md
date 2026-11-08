# Crystal

Crystal is a single static Rust binary that opens an age-encrypted Markdown
vault in a full-screen shell, decrypts notes only into locked memory,
and never writes plaintext to disk.

> **Status: in development, not ready for real notes.** Phases 0 to 2
> (skeleton, hardening, terminal guard, crypto and vault format) are done;
> Phase 3 (the Crystal shell and viewer) is in progress, after a redesign
> (decisions 0032 to 0036). Crystal becomes usable for daily notes after
> Phase 4. See [Roadmap and status](development/roadmap.md).

## How to read this book

- **Design** explains what Crystal protects, against whom, and how: start
  with the [overview](design/overview.md) and the
  [threat model](design/threat-model.md).
- **User guide** covers setting up the host and installing the binary.
- **Reference** lists every syscall Crystal makes and why; the seccomp
  allowlist is built from it.
- **Decisions** records each important design choice, its alternatives and
  its consequences, in the order it was made.
- **Development** has the roadmap and the test strategy.

The book is built from the `docs/` directory of the
[repository](https://github.com/tuna-guven/crystal). Code comments and
error messages refer to chapters by their path there, for example
`docs/design/startup.md`.
