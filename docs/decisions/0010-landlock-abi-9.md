# 0010 Landlock ABI 9 required, no fallback

- **Date:** 2026-11-06
- **Status:** Accepted

## Context

Landlock gains rights with each ABI version. ABI 9 adds `RESOLVE_UNIX`,
which blocks connecting to pathname UNIX sockets such as D-Bus and
Wayland; the signal and abstract-socket scopes need ABI 6. A sandbox that
silently drops rights the kernel lacks gives a false sense of safety.

## Decision

Build the ruleset with the `landlock` crate in hard-requirement mode.
Below ABI 9, or without Landlock, Crystal refuses to start, naming the
running and the required ABI. There is no weaker mode and no override
flag. No raw Landlock syscalls (they would need `unsafe`). See
[Sandboxing, 5.4](../design/sandboxing.md#54-fallback-policy).

## Alternatives

- **Best-effort mode:** runs with a partial sandbox without saying so.
- **An override flag:** would be used, and then the sandbox is optional.

## Consequences

Crystal needs a recent kernel (secureblue has one); CI needs a runner
with ABI 9 (#45).
