# Threat model

Crystal defends notes at rest and during use against storage, sync, theft and malicious content; it does not defend against root, kernel compromise or a hostile terminal emulator.

**Assets:** note content; note metadata (titles, links, tags, note count, sizes); the config file (vault names, locations, pinned ids); the vault identity key; the user's passphrase or hardware key.

| Adversary | Capability | Scope | Primary defense |
| --- | --- | --- | --- |
| A1 Storage or sync attacker (cloud, Syncthing peer, stolen backup) | Read, modify, delete, reorder, roll back ciphertext files | In | age AEAD over the whole vault, embedded vault\_id, generation counter, Padmé padding; against offline guessing of a copied `identity.age`, a passphrase of at least 20 characters ([0028](../decisions/0028-passphrase-minimum.md)) under scrypt log N 18 ([0027](../decisions/0027-scrypt-fixed.md)) |
| A2 Device thief, machine off | Full disk read | In | No plaintext ever written; full-disk encryption assumed as a second layer |
| A3 Malicious note content (imported, synced, pasted) | Terminal escape injection, parser bugs | In | Sanitizer on every text node, fuzzed parser and renderer, sandbox limits impact |
| A4 Supply chain (malicious or compromised crate) | Arbitrary code in the process | In | Minimal deps, cargo-vet, cargo-deny, sandbox blocks network and exec |
| A5 Same-user malware, vault locked | Read files, log keystrokes, replace or shadow `~/.local/bin/crystal`, write `config.age` | Partial | Notes stay encrypted; hardware key with touch defeats passphrase logging; optional immutable binary ([Installation](../guide/installation.md)); config paths checked as untrusted ([Config file](storage.md#config-file)) |
| A6 Same-user malware, vault unlocked | ptrace, /proc reads, screen capture | Partial | Non-dumpable process blocks ptrace and /proc/pid/mem; screen capture not preventable |
| A7 Root, kernel exploit, cold boot, evil maid | Everything | Out | Handled at OS level (verified boot, TPM, hardened kernel) |
| A8 Shoulder surfing, screen recording | Sees the screen | Out | Idle auto-lock reduces exposure |

**Trust boundaries:** the kernel; the terminal emulator (it receives every rendered character); the age crate; the compiled binary and its build chain; the installed copy in `~/.local/bin`, which is user-writable unless marked immutable ([Installation](../guide/installation.md)).

**Security properties Crystal guarantees**

- Confidentiality of note content and titles at rest
- Integrity of each note and of the vault as a set (no swapped, injected or deleted notes go unnoticed)
- Rollback detection relative to the newest state this device has seen ([Storage, Rollback](storage.md#rollback)); a same-user process (A5) can lower the plaintext counters, which is accepted
- No plaintext persisted by Crystal itself, including on crash

**Accepted leaks:** the vault's Padmé size bucket and when it was last saved. Note count, note sizes and which note changed stay hidden.
