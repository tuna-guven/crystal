# Decision log

Every decision that matters for Crystal's security, design or ease of use
gets a short record here: cryptography, file formats, sandboxing,
hardening, dependencies, Linux integration, the threat model, the build
and release process. Implementation details do not.

Records are numbered in the order they were made and are never rewritten.
When a later decision replaces one, the old record gets the status
*Superseded by NNNN* and the new one links back.

Each record has the same parts:

- **Status**: Accepted, Superseded by NNNN, or Proposed.
- **Context**: the problem and the forces at play.
- **Decision**: what Crystal does.
- **Alternatives**: what was considered and why it lost.
- **Consequences**: what follows, good and bad.

Records 0001 to 0010 were written down after the fact, when this log was
started (2026-11-06), from the design document and the code; their dates
are when the decision was made.

To add one: copy the shape of an existing record, take the next number,
add it to `docs/SUMMARY.md`, and update the design chapter it affects in
the same commit.
