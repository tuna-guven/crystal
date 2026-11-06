# Installation

Crystal is a per-user executable at `~/.local/bin/crystal`. Installing or updating it needs no root, and nothing goes into `/usr/local` (`/var/usrlocal` on secureblue) or the OS image.

```sh
cargo build --locked --release
install -m 0555 target/x86_64-unknown-linux-musl/release/crystal ~/.local/bin/crystal
```

**What this gives up.** Every process running as you can write to `~/.local/bin`, so same-user malware (A5) can replace or shadow the binary. Every protection inside Crystal would then protect the fake, and the fake can ask for your passphrase. A passphrase phished this way works forever; with a hardware key, a fake gets only the unlocks you confirm by touch.

**Optional immutability.** `run0 chattr +i ~/.local/bin/crystal`. Setting or clearing the flag needs `CAP_LINUX_IMMUTABLE`, so no process running as you can overwrite, delete, rename or replace the file. To update, run `run0 chattr -i`, then `install` and `restorecon`, then `run0 chattr +i`; `install` fails on an immutable target, which is the point, and the label must be set before the flag, since an immutable file cannot be relabeled. The flag does not stop the parent directory from being renamed and recreated (`mv ~/.local/bin ~/.local/bin.old; mkdir ~/.local/bin`). For the full guarantee, also run `run0 chattr +i ~/.local/bin`, which blocks every other per-user install into that directory too.

**Shadowing.** Directories earlier in `PATH` (such as `~/.cargo/bin`) and shell startup files can still run a different `crystal`. Use an absolute-path alias (`alias crystal="$HOME/.local/bin/crystal"`) in a `~/.bashrc` that is itself immutable.

**No self-check.** Crystal does not check its own immutable flag or hash at startup: a replaced binary would simply skip the check, so it would prove nothing.

**SELinux.** The policy labels `~/.local/bin/crystal` as `crystal_exec_t`. A new install creates a new file with the default label, so relabel after every install ([Sandboxing](../design/sandboxing.md)); otherwise Crystal runs outside `crystal_t`.
