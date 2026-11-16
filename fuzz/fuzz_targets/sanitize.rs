//! Fuzz target `sanitize`: see `crystal::fuzz::sanitize` (src/fuzz.rs).
#![no_main]
#![forbid(unsafe_code)]

libfuzzer_sys::fuzz_target!(|data: &[u8]| crystal::fuzz::sanitize(data));
