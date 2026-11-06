# 0017 Documentation as an mdBook from docs/

- **Date:** 2026-11-06
- **Status:** Accepted

## Context

The design lived in one long `DESIGN.md` plus three other files, and
decisions made while implementing lived only in commit messages and
issues. A security tool is only as credible as the reasons behind it.

## Decision

`docs/` is the source of an mdBook (`book.toml` at the repository root,
output in `target/book`). `DESIGN.md` was split into chapters with its
text unchanged; the old file now only maps sections to chapters. This
decision log records every important decision. CI builds the book on
every push and publishes it to GitHub Pages from `main`, using only
GitHub's own actions pinned by commit SHA and the official mdBook release
binary pinned by version and SHA-256. Every change that makes such a
decision updates the book in the same commit (CLAUDE.md).

## Alternatives

- **A separate book directory that copies `docs/`:** two copies drift.
- **Keep `DESIGN.md` whole and `{{#include}}` parts:** keeps one huge file
  and hides the structure.
- **Third-party actions to install mdBook:** an extra party in the supply
  chain.

## Consequences

Paths in code comments and error messages changed from `DESIGN.md` to
chapter paths. Older issues still say "DESIGN.md"; `docs/DESIGN.md`
maps them. The decision log will be reorganized after the first release.
