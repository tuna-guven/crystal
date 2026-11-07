# 0008 Locked memory floor of 256 MiB

- **Date:** 2026-11-06 (Phase 1)
- **Status:** Superseded by [0026](0026-memlock-floor-512.md)

## Context

`mlockall` keeps plaintext out of swap and hibernation images. If the
locked-memory budget runs out mid-session, an allocation fails and Rust
aborts, skipping zeroization and the save.

## Decision

Raise the soft `RLIMIT_MEMLOCK` to the hard limit, refuse to start if the
hard limit is below 256 MiB, then `mlockall(MCL_CURRENT | MCL_FUTURE |
MCL_ONFAULT)`. There is no override. The largest plaintext Crystal
encrypts is 32 MiB, well inside the floor. See
[Setup, Memory locking](../guide/setup.md#memory-locking-required).

## Alternatives

- **Lock only secret buffers:** misses copies made by libraries.
- **Run unlocked with a warning:** plaintext can reach swap.

## Consequences

Most distributions default to 8 MiB, so users must raise the limit once
(a `limits.d` or systemd setting). CI raises it for the test step.
