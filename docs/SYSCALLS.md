# Syscalls

Every syscall Crystal needs after the sandbox is applied, and why. The
seccomp allowlist (DESIGN.md, "Process hardening and sandboxing") must match
this table exactly: no syscall is allowed without a row here.

Add a row in the same change that first needs the syscall. Arguments that are
restricted by the filter (for example `ioctl` request numbers) go in the
"Restriction" column.

| Syscall | Used by | Why | Restriction |
| --- | --- | --- | --- |
