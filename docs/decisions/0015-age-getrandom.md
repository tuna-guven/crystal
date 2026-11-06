# 0015 age's random source must be allowed by seccomp

- **Date:** 2026-11-06 (#8)
- **Status:** Accepted

## Context

Every save creates a new age file key, nonce and ephemeral key, so age
needs randomness inside the sandbox. age uses `rand_core`'s `OsRng`
through `getrandom` 0.2. On musl, `getrandom` 0.2 probes the syscall once
with a zero-length buffer; if that returns `ENOSYS` or `EPERM`, it falls
back to opening `/dev/urandom`.

## Decision

The seccomp filter allows `getrandom` (any length, flags 0) and never
answers it with an errno. Crystal's own randomness goes through
`crypto::fill_random` (rustix, flags 0) for the same reason. See
[Syscalls](../reference/syscalls.md).

## Alternatives

- **Return `EPERM` for `getrandom`:** silently switches age to
  `/dev/urandom`, which Landlock denies, so saves fail.

## Consequences

`getrandom` is on the allowlist from the start; the zero-length probe is
expected in seccomp logs.
