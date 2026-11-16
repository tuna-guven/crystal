//! Fuzz target `age`: see `crystal::fuzz::age` (src/fuzz.rs).
#![no_main]
#![forbid(unsafe_code)]

libfuzzer_sys::fuzz_target!(|data: &[u8]| crystal::fuzz::age(data));
