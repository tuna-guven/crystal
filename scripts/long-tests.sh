#!/bin/sh
# Long property-test runs (docs/development/testing.md, "Property tests"):
# the same tests `cargo test` runs with proptest's default 256 cases, here
# with far more, in a release build. Run before a release, and weekly in CI
# (.github/workflows/long-tests.yml). Takes about 7 minutes on a desktop.
#
# Needs RLIMIT_MEMLOCK at Crystal's floor (512 MiB), like `cargo test`.
set -eu

# Untrusted text: the renderer, sanitizer, wrapping, names, key decoding
# and the vault codec. About 1 to 2.5 minutes each per million cases.
for module in md::render md::safe ui::wrap model::name shell::keys vault::codec; do
    echo "== $module: 1000000 cases"
    PROPTEST_CASES=1000000 cargo test --locked --release --bin crystal "$module"
done

# Padme padding allocates up to megabytes per case: fewer cases.
echo "== vault::padme: 10000 cases"
PROPTEST_CASES=10000 cargo test --locked --release --bin crystal vault::padme
