#!/bin/sh
# Imports the age test vectors of C2SP/CCTV
# (https://github.com/C2SP/CCTV/tree/main/age/testdata) into
# tests/corpus/age-cctv/, for src/age_v1/cctv.rs (decision 0042).
#
# A developer script, run by hand when the vectors should be updated;
# Crystal itself has no network code. It fetches one pinned commit (or the
# one given as $1) and copies the vectors byte for byte as published; the
# ones CCTV ships zlib-compressed are inflated by the test (miniz_oxide,
# a dev-dependency). The vectors are 0BSD / CC0 / Unlicense (CCTV README):
# no attribution needed.
#
# Needs git. Usage: scripts/import-cctv.sh [COMMIT]
set -eu

REPO=https://github.com/C2SP/CCTV.git
COMMIT=${1:-b88c4b206254ac543df240097e0f1fcc6a664821}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
DEST="$ROOT/tests/corpus/age-cctv"

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Fetch exactly this commit, no history.
git -C "$WORK" init --quiet repo
git -C "$WORK/repo" fetch --quiet --depth 1 "$REPO" "$COMMIT"
git -C "$WORK/repo" checkout --quiet FETCH_HEAD
GOT=$(git -C "$WORK/repo" rev-parse HEAD)
[ "$GOT" = "$COMMIT" ] || { echo "fetched $GOT, not $COMMIT" >&2; exit 1; }

# Replace the corpus: only files this script wrote before.
rm -rf "$DEST"
mkdir -p "$DEST"
cp "$WORK/repo/age/testdata/"* "$DEST/"

{
    echo "C2SP/CCTV age test vectors, unchanged"
    echo "source: $REPO, directory age/testdata"
    echo "commit: $COMMIT"
    echo "license: 0BSD, CC0 1.0 or Unlicense (see the CCTV README)"
    echo "imported by: scripts/import-cctv.sh"
} > "$DEST/SOURCE"

echo "$(ls "$DEST" | wc -l) files in $DEST, $(du -sh "$DEST" | cut -f1)"
