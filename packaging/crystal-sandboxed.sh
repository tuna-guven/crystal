#!/bin/sh
# Optional outer sandbox for Crystal (docs/design/sandboxing.md, section 8;
# roadmap step S10): runs `crystal unlock NAME` inside bubblewrap, where
# the file system holds only the Crystal binary, this one vault and the
# state directory, with new user, PID, network, IPC, UTS and cgroup
# namespaces. Crystal's own Landlock, seccomp and SELinux domain apply
# inside it as always; this is one more layer, never a replacement.
#
# Usage: crystal-sandboxed.sh NAME [crystal debug options]
# Opens existing vaults only: run crystal init and rm
# directly.
set -eu

if [ "$#" -lt 1 ]; then
    echo "usage: crystal-sandboxed.sh NAME" >&2
    exit 2
fi
name=$1
shift
# Crystal's own name rule (1 to 64 of A-Z a-z 0-9 - _, starting with a
# letter or digit), checked here too, so a bad name binds nothing.
case $name in
    '' | [!A-Za-z0-9]* | *[!A-Za-z0-9_-]*)
        echo "invalid vault name" >&2
        exit 2
        ;;
esac
if [ "${#name}" -gt 64 ]; then
    echo "invalid vault name" >&2
    exit 2
fi

bin=$HOME/.local/bin/crystal
vault=$HOME/.local/share/crystal/$name
state=$HOME/.local/state/crystal
# Not created here: a directory made by this shell would get the wrong
# SELinux label. crystal init creates both.
for dir in "$vault" "$state"; do
    if [ ! -d "$dir" ]; then
        echo "no such vault, or no state directory: run crystal init $name first" >&2
        exit 1
    fi
done

# --unshare-all: new user, PID, network, IPC, UTS and cgroup namespaces.
# --clearenv: Crystal reads only HOME (its environment allowlist).
# No /tmp, no /etc: Crystal is static and uses neither. No D-Bus, Wayland,
# X11 or audio sockets. --new-session is not used: it breaks job control,
# and TIOCSTI is killed by Crystal's seccomp filter anyway.
# --perms 0700 --dir recreates an empty ~/.local/share/crystal, so the
# other vaults do not exist inside the sandbox.
exec bwrap \
    --unshare-all --die-with-parent \
    --clearenv --setenv HOME "$HOME" \
    --ro-bind "$bin" "$bin" \
    --proc /proc --dev /dev \
    --perms 0700 --dir "$HOME/.local/share/crystal" \
    --bind "$vault" "$vault" \
    --bind "$state" "$state" \
    "$bin" unlock "$@" "$name"
