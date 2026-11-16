#!/bin/sh
# Builds and signs a release (decision 0069; roadmap step R3): the
# reproducible binary (packaging/build-release.sh), its SHA256SUMS, and
# SHA256SUMS.sig, an SSH signature by a release key that lives on a
# YubiKey (ed25519-sk: every signature needs a touch). The signature is
# checked against packaging/allowed_signers before the script ends.
#
# Usage: sign-release.sh KEY   (for example ~/.ssh/crystal-release)
# Output: target/release-files/{crystal,SHA256SUMS,SHA256SUMS.sig}
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd -P)
key=${1:?usage: sign-release.sh KEY}
cd "$repo"

# A release is a commit: anything uncommitted would make the binary one
# nobody can rebuild from the repository.
if [ -n "$(git status --porcelain)" ]; then
    echo "uncommitted changes: commit first, then sign" >&2
    exit 1
fi

packaging/build-release.sh "$repo"
out="$repo/target/release-files"
rm -rf "$out"
mkdir -p "$out"
cp target/x86_64-unknown-linux-musl/release/crystal "$out/crystal"
(cd "$out" && sha256sum crystal >SHA256SUMS)
echo "commit $(git rev-parse HEAD)"

echo "Touch the YubiKey to sign." >&2
ssh-keygen -Y sign -f "$key" -n crystal-release "$out/SHA256SUMS"
ssh-keygen -Y verify -f packaging/allowed_signers -I crystal-release \
    -n crystal-release -s "$out/SHA256SUMS.sig" <"$out/SHA256SUMS"
ls -l "$out"
