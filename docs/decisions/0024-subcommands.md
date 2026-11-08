# 0024 Subcommands: crystal init, add, remove

- **Date:** 2026-11-06
- **Status:** Accepted; `add` and `remove` are superseded by
  [0032](0032-fixed-locations.md) (`rm`, `check-code`)

## Context

The commands that change the config were flags: `crystal --init NAME`,
`--add NAME DIR`, `--remove NAME`. Flags can appear anywhere and
combine, which needed a "conflicting modes" rule, and they read like
options rather than actions.

## Decision

The first word may be a command: `crystal init NAME [--at DIR]`,
`crystal add NAME DIR`, `crystal remove NAME`, `crystal help`. Without one,
`crystal [--lock-after MINUTES] NAME` opens a vault. The words `init`,
`add`, `remove` and `help` are reserved: `VaultName` refuses them, so
`crystal init` can never mean "open the vault named init", and a command
word after the first word is a refused name, not a second command. The old
flags are unknown arguments. `-h` and `--help` still work.

Records written before this one (0004 to 0022) use the old flag spelling;
they are not rewritten.

## Alternatives

- **Keep the flags:** works, but needs the conflicting-modes rule and reads
  less clearly.
- **`crystal open NAME` for opening:** no ambiguity without reserved names,
  but the most common command gets longer.
- **Allow reserved names and disambiguate by position:** `crystal init`
  alone would have two meanings.

## Consequences

- Four vault names are unavailable. Nothing has been released, so no
  existing vault can have one.
- Scripts and habits from before this change must use the new spelling.
