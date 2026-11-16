//! Fuzz target `vault`: see `crystal::fuzz::vault` (src/fuzz.rs).
#![no_main]
#![forbid(unsafe_code)]

libfuzzer_sys::fuzz_target!(|data: &[u8]| crystal::fuzz::vault(data));
