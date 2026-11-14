# 0050 Auto-lock counts the time the computer sleeps

- **Date:** 2026-11-14 (Phase 5 review)
- **Status:** Accepted. Replaces how [0048](0048-lock-and-auto-lock.md)
  measures the idle time ("measured on the monotonic clock"); the rest of
  0048 stays in force.

## Context

[0048](0048-lock-and-auto-lock.md) measured the 5 minutes without input
on the monotonic clock (std `Instant`, `CLOCK_MONOTONIC`), and the input
wait's timeout runs on it too. That clock stops while the computer
sleeps. A laptop that sleeps after 1 minute idle wakes with Crystal still
unlocked, and auto-lock comes only after the 4 minutes left, however long
it slept: an unattended, sleeping laptop is exactly the case auto-lock is
for.

## Decision

- **The idle time is the longer of two clocks:** the monotonic clock and
  the wall clock (std `SystemTime`, `CLOCK_REALTIME`), both read at the
  last key or paste. The wall clock keeps running while the computer
  sleeps, so sleep counts. Setting the wall clock back cannot delay a
  lock, since the monotonic clock still counts; setting it forward only
  locks sooner.
- **Each input wait lasts at most 5 seconds,** then auto-lock is checked
  again. After a wake, Crystal locks within 5 seconds once the idle time
  has passed.
- Still fixed at 5 minutes, with no setting; what counts as input is as in
  0048.

## Alternatives

- **`CLOCK_BOOTTIME`,** which counts sleep and cannot be set: needs
  rustix's `time` feature (a dependency change) for the same result.
- **A `timerfd` on `CLOCK_BOOTTIME`:** wakes exactly on time, but adds
  two syscalls and an fd for a 5-second gain.
- **Keep the monotonic clock:** a sleeping laptop stays unlocked on wake.

## Consequences

- An idle Crystal wakes every 5 seconds; each wake draws a frame, which
  sends only a style reset when nothing changed.
- No new syscall: `clock_gettime` with `CLOCK_REALTIME` was already used
  for note times ([Syscalls](../reference/syscalls.md)).
- Crystal cannot lock *during* sleep (it does not run): while the
  computer sleeps, the notes stay in its memory, and only the OS's own
  screen lock guards them until wake.
