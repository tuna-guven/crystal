#!/bin/sh
# Long property-test runs (docs/development/testing.md, "Property tests"):
# the same tests `cargo test` runs with proptest's default 256 cases, here
# with far more, in a release build. Run before a release, and weekly in CI
# (.github/workflows/long-tests.yml). Takes about 32 minutes on a desktop
# (age_v1 about 14 of them), about 45 in CI (timeout 90).
#
# Needs RLIMIT_MEMLOCK at Crystal's floor (2 GiB), like `cargo test`. The unit
# tests live in the library since step S11a, hence --lib (with --bin
# crystal, which has no tests, a run would pass having run nothing).
set -eu

# Untrusted text: the renderer, sanitizer, wrapping, names, key decoding,
# the vault codec and the editor (buffer and keys). About 1 to 2.5 minutes
# each per million cases (the editor about 9).
for module in md::render md::safe ui::wrap model::name vault::codec editor; do
    echo "== $module: 1000000 cases"
    PROPTEST_CASES=1000000 cargo test --locked --release --lib "$module"
done

# Every case of clean_pastes_report_nothing starts a paste, which allocates
# and zeroizes the full 8 MiB paste buffer (about 5 ms a case, 80 minutes
# per million): fewer cases for it, a million for the rest of shell::keys.
echo "== shell::keys: 1000000 cases"
PROPTEST_CASES=1000000 cargo test --locked --release --lib shell::keys -- --skip clean_pastes_report_nothing
echo "== shell::keys::tests::clean_pastes_report_nothing: 10000 cases"
PROPTEST_CASES=10000 cargo test --locked --release --lib shell::keys::tests::clean_pastes_report_nothing -- --exact

# Crystal's own age v1 code (src/age_v1, decision 0042): base64, Bech32,
# header, STREAM, scrypt and X25519 stanzas, whole files. Untrusted file
# bytes: round trips, flipped bits, truncation and extension, random input.
echo "== age_v1: 1000000 cases"
PROPTEST_CASES=1000000 cargo test --locked --release --lib age_v1

# Padme padding allocates up to megabytes per case: fewer cases.
echo "== vault::padme: 10000 cases"
PROPTEST_CASES=10000 cargo test --locked --release --lib vault::padme
