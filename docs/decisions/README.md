# Decision log

Every decision that matters for Crystal's security, design or ease of use
gets a short record here: cryptography, file formats, sandboxing,
hardening, dependencies, Linux integration, the threat model, the build
and release process. Implementation details do not.

Records are numbered in the order they were made and are never rewritten
for a new decision; a new decision gets a new record. When a later
decision replaces part of a record, the old record's status line says
which part, and the new one names it. When a record is replaced
entirely, it is deleted, and the record that replaces it states
everything still in force, so the log holds only what applies today. Git
history keeps the deleted records; numbers are never reused, so gaps are
expected.

Each record has the same parts:

- **Status**: Accepted (noting any part replaced by a later record), or Proposed.
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
