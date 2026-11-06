# 0006 Per-user install in ~/.local/bin

- **Date:** 2026-11-06
- **Status:** Accepted

## Context

On secureblue (Fedora Atomic) the OS image is read-only and `/usr/local`
is `/var/usrlocal`. Installing system-wide needs root for every update.

## Decision

Crystal installs per user to `~/.local/bin/crystal` with no root. An
optional `chattr +i` (needs root once per update) makes the file
immutable. Crystal does not check its own hash or flag at startup. See
[Installation](../guide/installation.md).

## Alternatives

- **System-wide install:** root for every update.
- **A self-check at startup:** a replaced binary would skip it, so it
  proves nothing.

## Consequences

Same-user malware (A5) can replace or shadow the binary unless it is
immutable, and a fake can ask for the passphrase. A hardware key limits
a fake to the unlocks you confirm by touch. The SELinux label must be
restored after every install, before setting the immutable flag.
