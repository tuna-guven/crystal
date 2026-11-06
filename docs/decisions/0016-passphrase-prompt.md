# 0016 Passphrase prompt without echo or reallocation

- **Date:** 2026-11-06 (#9)
- **Status:** Accepted

## Context

The passphrase must never reach the screen, scrollback, argv, the
environment, or a freed heap block. `rpassword` is on the avoid list.
Keys typed before raw mode is on are echoed by the terminal itself.

## Decision

`TerminalGuard::read_passphrase` reads `/dev/tty` in raw mode on the
alternate screen:

- No echo, not even asterisks (they show the length).
- A `Zeroizing` buffer of 1024 bytes, allocated once; longer input is
  refused, so the buffer never grows. Deleted bytes are zeroed at once.
  On Enter the buffer itself becomes the `Passphrase`, without a copy.
- Keys typed before the prompt appeared are discarded (`tcflush`), since
  the terminal already echoed them.
- Ctrl+C, Esc or an exit signal cancels.
- `--init` asks twice and compares in constant time with an XOR fold, so
  no new dependency (`subtle`) is needed.

## Alternatives

- **`rpassword`:** less control over buffers and echo.
- **Asterisks:** leak the length to anyone watching.
- **Using `subtle` directly:** it is in age's tree, but a direct
  dependency needs approval and the fold is a few lines.

## Consequences

The user gets no visual feedback while typing. Esc also starts arrow
keys, so any arrow key cancels the prompt.
