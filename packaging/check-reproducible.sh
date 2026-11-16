#!/bin/sh
# Phase 8 acceptance (decision 0067; roadmap step R1): two clean builds of
# the committed HEAD, in two directories of different names and lengths
# and with Cargo's home under two different paths,
# must give the same SHA-256, and the binary must not hold the builder's
# home directory or user name in a path. Builds offline from the crates already in
# Cargo's cache. Uncommitted changes are not part of the check.
#
# Usage: check-reproducible.sh   (from anywhere in the checkout)
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd -P)
build="$repo/packaging/build-release.sh"
work="$repo/target/repro"
rm -rf "$work"
mkdir -p "$work"

# hash_of DIR [CARGO_HOME]
hash_of() {
    git clone --quiet --no-hardlinks "$repo" "$1"
    CARGO_HOME=${2:-${CARGO_HOME:-$HOME/.cargo}} CARGO_NET_OFFLINE=true \
        "$build" "$1" >"$1.log" 2>&1 || {
        tail -20 "$1.log" >&2
        exit 1
    }
    tail -1 "$1.log" | cut -d' ' -f1
}

# The second build also sees Cargo's home under another path, as another
# user would: a directory whose registry links to the same crate cache.
other_home="$work/another-cargo-home"
mkdir -p "$other_home"
ln -s "${CARGO_HOME:-$HOME/.cargo}/registry" "$other_home/registry"
a=$(hash_of "$work/a")
b=$(hash_of "$work/second-build-in-a-longer-directory" "$other_home")
echo "build 1: $a"
echo "build 2: $b"

bin="$work/a/target/x86_64-unknown-linux-musl/release/crystal"
# Paths only: a login name can be an ordinary word ("user").
home_resolved=$(cd "$HOME" && pwd -P)
leaks=$(strings "$bin" |
    grep -c -F -e "$HOME" -e "$home_resolved" -e "/home/" -e "/$(id -un)/" || true)
echo "home paths or the user name as a path in the binary: $leaks"

if [ "$a" = "$b" ] && [ "$leaks" -eq 0 ]; then
    echo "reproducible"
else
    echo "NOT reproducible" >&2
    exit 1
fi
