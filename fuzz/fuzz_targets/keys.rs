//! Fuzz target `keys`: see `crystal::fuzz::keys` (src/fuzz.rs).
#![no_main]
#![forbid(unsafe_code)]

libfuzzer_sys::fuzz_target!(|data: &[u8]| crystal::fuzz::keys(data));
