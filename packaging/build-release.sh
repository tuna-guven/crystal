#!/bin/sh
# Builds Crystal's release binary reproducibly (decision 0067; roadmap
# step R1): the same source and toolchain give the same bytes, whoever
# builds it and wherever the source lies.
#
# Usage: build-release.sh [SOURCE_DIR]   (default: this script's checkout)
#
# rustc writes source paths into the binary (panic locations of Crystal,
# its dependencies and the standard library). They are mapped to fixed
# names, so the build directory and the user's home and name do not end
# up in it:
# - the checkout to /crystal;
# - Cargo's registry to /cargo/registry/src, under the path Cargo uses and
#   its resolved form (on secureblue /home is a symlink to /var/home);
# - the toolchain's rust-src, to which rustc maps its own /rustc/<commit>
#   paths back when that component is installed, to /rustc/<commit> again.
# Cargo's own trim-paths option is not stable on the pinned toolchain.
set -eu

src=$(cd "${1:-$(dirname "$0")/..}" && pwd -P)
cd "$src"
registry="${CARGO_HOME:-$HOME/.cargo}/registry/src"
registry_resolved=$(cd "$registry" && pwd -P)
rust_src="$(rustc --print sysroot)/lib/rustlib/src/rust"
commit=$(rustc -vV | sed -n 's/^commit-hash: //p')

# One flag per field, separated by 0x1f, so a path with spaces stays whole.
s=$(printf '\037')
flags="--remap-path-prefix=$src=/crystal"
flags="$flags$s--remap-path-prefix=$registry=/cargo/registry/src"
flags="$flags$s--remap-path-prefix=$registry_resolved=/cargo/registry/src"
flags="$flags$s--remap-path-prefix=$rust_src=/rustc/$commit"
CARGO_ENCODED_RUSTFLAGS=$flags
export CARGO_ENCODED_RUSTFLAGS
unset RUSTFLAGS

cargo build --locked --release
sha256sum target/x86_64-unknown-linux-musl/release/crystal
