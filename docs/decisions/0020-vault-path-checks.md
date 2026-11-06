# 0020 Vault paths: walk from /, compare by inode

- **Date:** 2026-11-06 (#12)
- **Status:** Accepted

## Context

`config.age` maps vault names to directories. It is authenticated only as
strongly as its recipient ([0005](0005-untrusted-config.md)): under a
public-key recipient, anyone can write a config that decrypts. A crafted
path could point Crystal at `~/.ssh`, `$HOME`, the config or state
directory, or a directory another user can swap out, and from Phase 6 the
vault directory becomes a Landlock rule, so a bad path would widen the
sandbox.

## Decision

`vault::VaultDir::open_checked` opens a vault path only if:

1. its raw bytes are absolute, under PATH_MAX, without NUL, empty, `.` or
   `..` components (checked on bytes, since `Path::components` normalises
   `.` and `//` away);
2. a walk from `/`, one component at a time with `O_NOFOLLOW`, succeeds:
   no symlink anywhere;
3. every directory on the way is owned by root or by you and is not group-
   or world-writable, with no exception for sticky directories;
4. no directory on the way is the config or state directory, the vault is
   not `$HOME`, and it is not an ancestor of the config or state directory,
   all compared by `(st_dev, st_ino)` of open fds;
5. the vault directory is yours, mode 0700, with a valid `VERSION`.

The owner and permission rule for intermediate directories is one helper
(`is_safe_parent`) shared with the walks to the config and state
directories, which accept only your own directories.

## Alternatives

- **Canonicalise the path (`realpath`) and compare strings:** a race
  between resolving and opening, and string comparison misses bind mounts
  and other aliases.
- **Allow sticky world-writable parents (`/tmp`):** others cannot rename
  your entries there, but can pre-create names and fill the directory; the
  design forbids any group- or world-writable parent, and refusing is the
  simpler rule to audit.
- **Follow symlinks in the path:** whoever can change the link target can
  redirect the vault.

## Consequences

- Stored vault paths must be fully resolved. On Fedora Atomic, `/home` is
  a symlink to `/var/home`, so `--init` and `--add` (#13, #14) must store
  the resolved path; a hand-written `/home/...` path is refused with a
  message saying paths must be fully resolved.
- A vault cannot live under `/tmp`, a group-writable shared directory, or
  any path with a symlink in it.
- Errors give the position of the failing component, never the path.
