# 0026 Locked memory floor of 512 MiB

- **Date:** 2026-11-07 (Phase 2 review)
- **Status:** Accepted; supersedes [0008](0008-memlock-floor.md)

## Context

[0008](0008-memlock-floor.md) set the `RLIMIT_MEMLOCK` floor at 256 MiB.
Since [0025](0025-scrypt-budget.md), scrypt may use at most half of the
limit, so at that floor passphrase files got log N 17 (128 MiB): half the
cost of age's one-second default on a fast machine (log N 18, 256 MiB).

`identity.age` is the vault's secret key, encrypted to the passphrase. It
sits in the vault directory and goes wherever the vault is synced or
backed up, so its scrypt cost is what protects every note against an
attacker who never touches the device. `config.age` uses the same
passphrase, so it adds nothing beyond that. The floor also decides what
every device can open: a file must fit in half the limit of the device
that decrypts it.

## Decision

Refuse to start if the hard limit is below 512 MiB
(`harden::MEMLOCK_FLOOR`). Everything else in 0008 is unchanged: raise
the soft limit to the hard limit, `mlockall(MCL_CURRENT | MCL_FUTURE |
MCL_ONFAULT)`, no override. Half the floor, 256 MiB, holds log N 18, so
any device that starts Crystal can open a passphrase file at log N 18.

The floor is a fixed number, not derived from the machine's RAM, its
limit or its speed: it is a promise that holds on every device, and a
synced vault must open on all of them.

## Alternatives

- **Keep 256 MiB:** log N 17 at the floor; half the guessing cost.
- **Adaptive floor (RAM, limit or timing):** a device could then start
  with less than another device's files need, and the guarantee that
  every device opens every vault is lost. RAM never limits Crystal; the
  memlock limit does.
- **Floor 1 GiB or 2 GiB (log N 19 or 20):** stronger, but every device
  needs it, and each step doubles unlock time; the 20-character
  passphrase minimum adds far more.
- **"Limit minus a reserve" instead of half:** 384 MiB would also hold
  log N 18, but the reserve must be re-checked as later phases keep more
  in memory while scrypt runs; half stays simple.

## Consequences

Users who set the limit for 0008 (262144 KiB) must raise it to
524288 KiB once ([Setup](../guide/setup.md#memory-locking-required));
until then Crystal refuses to start with a message saying so. CI pins the
limit at exactly 512 MiB. Any process running as the user can lock up to
512 MiB, a minor denial-of-service risk on a single-user desktop.
