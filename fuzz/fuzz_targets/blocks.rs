//! Fuzz target `blocks`: see `crystal::fuzz::blocks` (src/fuzz.rs).
#![no_main]
#![forbid(unsafe_code)]

libfuzzer_sys::fuzz_target!(|data: &[u8]| crystal::fuzz::blocks(data));
