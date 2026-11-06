# 0009 Signals through self-pipes, no threads

- **Date:** 2026-11-06 (Phase 1)
- **Status:** Accepted

## Context

A signal whose default action kills the process skips every `Drop`: the
terminal stays raw on the alternate screen and notes are not zeroized.
Signal handlers may only do async-signal-safe work. The sandbox denies
`clone` and `socketpair`.

## Decision

Every catchable signal whose default kills the process (and `SIGTSTP`)
gets a handler, through signal-hook's `low_level::pipe`, that writes one
byte to a non-blocking self-pipe. The main loop polls the pipes with the
tty and leaves through the normal exit path, so the terminal is restored
and memory zeroized. Faults, `SIGKILL` and the libc-reserved real-time
signals stay uncaught; [Plaintext leakage prevention, Exits that skip
cleanup](../design/leakage.md#exits-that-skip-cleanup) explains each.

## Alternatives

- **signal-hook's iterator or channel modules:** create a `socketpair` or
  a thread, both denied.
- **Restoring the terminal inside the handler:** not async-signal-safe.

## Consequences

Crystal stays single-threaded. A few exits remain that cannot clean up;
none of them writes plaintext to disk.
