//! Crystal's program. Everything is in the library (src/lib.rs), which
//! exports only `run`, so the fuzz targets in fuzz/ can reach the parsers
//! without the modules becoming public.

#![forbid(unsafe_code)]

fn main() -> std::process::ExitCode {
    crystal::run()
}
