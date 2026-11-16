//! Fuzz target `render`: see `crystal::fuzz::render` (src/fuzz.rs).
#![no_main]
#![forbid(unsafe_code)]

libfuzzer_sys::fuzz_target!(|data: &[u8]| crystal::fuzz::render(data));
