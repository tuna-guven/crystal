#!/bin/sh
# Long property-test runs (docs/development/testing.md, "Property tests"):
# the same tests `cargo test` runs with proptest's default 256 cases, here
# with far more, in a release build. Run before a release, and weekly in CI
# (.github/workflows/long-tests.yml). Takes about 21 minutes on a desktop
# (age_v1 about 14 of them).
#
# Needs RLIMIT_MEMLOCK at Crystal's floor (512 MiB), like `cargo test`.
set -eu

# Untrusted text: the renderer, sanitizer, wrapping, names, key decoding
# and the vault codec. About 1 to 2.5 minutes each per million cases.
for module in md::render md::safe ui::wrap model::name shell::keys vault::codec; do
    echo "== $module: 1000000 cases"
    PROPTEST_CASES=1000000 cargo test --locked --release --bin crystal "$module"
done

# Crystal's own age v1 code (src/age_v1, decision 0042): base64, Bech32,
# header, STREAM, scrypt and X25519 stanzas, whole files. Untrusted file
# bytes: round trips, flipped bits, truncation and extension, random input.
echo "== age_v1: 1000000 cases"
PROPTEST_CASES=1000000 cargo test --locked --release --bin crystal age_v1

# Padme padding allocates up to megabytes per case: fewer cases.
echo "== vault::padme: 10000 cases"
PROPTEST_CASES=10000 cargo test --locked --release --bin crystal vault::padme
