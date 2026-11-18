#!/bin/sh
# Publishes a release (decision 0071; roadmap step P2), run by the
# maintainer on the tagged commit:
#   1. the tree is clean, HEAD is TAG, and GitHub's TAG is the same commit;
#   2. CI's "Release build" run for TAG finished and printed its hash;
#   3. this machine builds the same commit (packaging/build-release.sh);
#   4. both hashes must be equal, or nothing is signed;
#   5. packaging/sign-release.sh signs SHA256SUMS (touch the YubiKey);
#   6. after a yes, `gh release create` uploads the three files.
#
# Usage: publish-release.sh TAG KEY   (for example v0.1.0 ~/.ssh/crystal-release)
set -eu

tag=${1:?usage: publish-release.sh TAG KEY}
key=${2:?usage: publish-release.sh TAG KEY}
repo=$(cd "$(dirname "$0")/.." && pwd -P)
cd "$repo"

fail() {
    echo "publish-release: $*" >&2
    exit 1
}

case $tag in
v[0-9]*) ;;
*) fail "a release tag looks like v0.1.0, not $tag" ;;
esac

# 1. The commit.
[ -z "$(git status --porcelain)" ] || fail "uncommitted changes"
commit=$(git rev-parse --verify --quiet "refs/tags/$tag^{commit}") ||
    fail "no local tag $tag"
[ "$(git rev-parse HEAD)" = "$commit" ] || fail "HEAD is not $tag: git checkout $tag"
remote=$(git ls-remote origin "refs/tags/$tag" "refs/tags/$tag^{}" |
    awk 'END { print $1 }')
[ "$remote" = "$commit" ] || fail "GitHub's $tag is not $commit: git push origin $tag"

# 2. CI's hash, from the run the tag push started.
run=$(gh run list --workflow release-build.yml --branch "$tag" --limit 1 \
    --json databaseId,status,headSha,conclusion \
    -q '.[] | "\(.databaseId) \(.status) \(.headSha) \(.conclusion)"')
[ -n "$run" ] || fail "no Release build run for $tag yet"
# The conclusion is empty while the run is going, hence last.
set -- $run
[ "$3" = "$commit" ] || fail "the latest run for $tag built $3, not $commit"
[ "$2" = completed ] || fail "CI is still building $tag: gh run watch $1"
[ "${4:-}" = success ] || fail "CI's build of $tag ended in ${4:-nothing}: gh run view $1"
ci=$(gh run view "$1" --log |
    grep -o "release $tag $commit sha256 [0-9a-f]\{64\}" |
    awk 'NR == 1 { print $5 }')
[ -n "$ci" ] || fail "no hash line in run $1"

# 3 and 4. The same bytes here, before anything is signed.
packaging/build-release.sh >/dev/null
here=$(sha256sum target/x86_64-unknown-linux-musl/release/crystal | cut -d' ' -f1)
echo "commit $commit"
echo "CI     $ci"
echo "here   $here"
[ "$here" = "$ci" ] || fail "the hashes differ: not signing"

# 5. Sign (rebuilds nothing: the build above is current).
packaging/sign-release.sh "$key"
out=target/release-files
[ "$(cut -d' ' -f1 "$out/SHA256SUMS")" = "$ci" ] ||
    fail "SHA256SUMS does not hold the compared hash"

# 6. Publish.
printf 'Publish %s with these three files on GitHub? [y/N] ' "$tag"
read -r answer
[ "$answer" = y ] || fail "not published"
gh release create "$tag" --verify-tag --title "Crystal ${tag#v}" \
    --notes "Commit $commit. SHA-256 of \`crystal\`: $ci (also built by CI, run $1). Verify the signature and hash as in docs/guide/installation.md, or rebuild the commit with packaging/build-release.sh." \
    "$out/crystal" "$out/SHA256SUMS" "$out/SHA256SUMS.sig"
