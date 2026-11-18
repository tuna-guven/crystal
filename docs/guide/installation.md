# Installation

Crystal is a per-user executable at `~/.local/bin/crystal`. Installing or updating it needs no root, and nothing goes into `/usr/local` (`/var/usrlocal` on secureblue) or the OS image.

## Getting a binary you can trust

A release is three files: `crystal`, `SHA256SUMS` and `SHA256SUMS.sig`, an
SSH signature of `SHA256SUMS` by one of the two release keys in
`packaging/allowed_signers` ([decision 0069](../decisions.md#0069)). Both
keys live on YubiKeys, so nobody can sign without one of them in hand.
Check the signature, then the hash, before you install:

```sh
ssh-keygen -Y verify -f allowed_signers -I crystal-release -n crystal-release \
    -s SHA256SUMS.sig < SHA256SUMS      # must say: Good "crystal-release" signature
sha256sum -c SHA256SUMS                 # must say: crystal: OK
```

Take `allowed_signers` from the repository, and check its two keys against
a copy published elsewhere, since someone who controls the repository
could swap both the binary and the keys. The fingerprints:

| Key | Fingerprint |
| --- | --- |
| `crystal-release-1` | `SHA256:P39EbN2yia7n1jQ/i5Gnw2lVor1RYSUmnXU6n+T6/6s` |
| `crystal-release-2` | `SHA256:b9cJShmQVnxQxRp8ntLHtb91/XXsavAhz+CoNMXdBXk` |

`ssh-keygen -l -f allowed_signers` does not read that file; compare the
keys by eye, or put each line's key into a `.pub` file and run
`ssh-keygen -l -f` on it.

**Or build it yourself.** The build is reproducible ([decision
0067](../decisions.md#0067)): the same commit gives the same bytes, so a
binary you build must match the published `SHA256SUMS`. You need only the
toolchain from `rust-toolchain.toml`, which rustup installs by itself.
The release's dependencies are that commit's `Cargo.lock`; `cargo audit`
there checks them for advisories ([decision 0072](../decisions.md#0072)).

```sh
git checkout <release commit>
packaging/build-release.sh              # prints the SHA-256; compare it
```

GitHub's CI builds every release tag the same way and prints its hash in
the run log of the "Release build" workflow ([decision
0071](../decisions.md#0071)): a third copy of the hash to compare.

## Installing

```sh
install -m 0555 crystal ~/.local/bin/crystal
restorecon -Fv ~/.local/bin/crystal     # SELinux: must show crystal_exec_t
```

(From a build of your own, the binary is
`target/x86_64-unknown-linux-musl/release/crystal`.) During development
`cargo install --path .` puts an unlabeled build in `~/.cargo/bin`
instead; that is not this install.

**What this gives up.** Every process running as you can write to `~/.local/bin`, so same-user malware (A5) can replace or shadow the binary. Every protection inside Crystal would then protect the fake, and the fake can ask for your passphrase. A passphrase phished this way opens the vault until you change it with `crystal passwd`, and copies made before that for good; with a hardware key, a fake gets only the unlocks you confirm by touch.

**Optional immutability.** `run0 chattr +i ~/.local/bin/crystal`. Setting or clearing the flag needs `CAP_LINUX_IMMUTABLE`, so no process running as you can overwrite, delete, rename or replace the file. To update, run `run0 chattr -i`, then `install` and `restorecon -F`, then `run0 chattr +i`; `install` fails on an immutable target, which is the point, and the label must be set before the flag, since an immutable file cannot be relabeled. The flag does not stop the parent directory from being renamed and recreated (`mv ~/.local/bin ~/.local/bin.old; mkdir ~/.local/bin`). For the full guarantee, also run `run0 chattr +i ~/.local/bin`, which blocks every other per-user install into that directory too.

**Shadowing.** Directories earlier in `PATH` (such as `~/.cargo/bin`) and shell startup files can still run a different `crystal`. Use an absolute-path alias (`alias crystal="$HOME/.local/bin/crystal"`) in a `~/.bashrc` that is itself immutable.

**No self-check.** Crystal does not check its own immutable flag or hash at startup: a replaced binary would simply skip the check, so it would prove nothing.

**SELinux.** The policy labels `~/.local/bin/crystal` as `crystal_exec_t`. Relabel after every install with `restorecon -F ~/.local/bin/crystal` and check with `ls -Z` ([Sandboxing, section 7](../design/sandboxing.md)): an install made before the module was loaded keeps `home_bin_t`, a customizable type a plain `restorecon` never changes, and Crystal then runs outside `crystal_t`.
