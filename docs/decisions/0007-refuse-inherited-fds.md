# 0007 Fail closed on inherited file descriptors

- **Date:** 2026-11-06 (Phase 1)
- **Status:** Accepted

## Context

Landlock does not restrict file descriptors opened before it, and seccomp
allows `read` and `write`. An inherited socket or directory fd would
bypass the sandbox, and the age plugin would inherit it too. Closing an
fd Crystal does not own needs `unsafe` (rustix has no safe
`close_range`).

## Decision

At startup step 2, before opening anything of its own, Crystal lists
`/proc/self/fd` and refuses to start if any fd above 2 is open. See
[Startup](../design/startup.md) and [Setup](../guide/setup.md).

## Alternatives

- **Close stray fds:** needs `unsafe` in the crystal crate, which is
  forbidden.
- **Ignore them:** a hole in the sandbox.

## Consequences

Some launchers leak fds and must be fixed or wrapped (GitHub's CI runner
does; the workflow closes them before `cargo test` rather than weakening
the check).
