//! The binary entry point.
//!
//! This file is intentionally tiny. All of shopli's real logic lives in the
//! *library* crate (`src/lib.rs`) so it can be unit-tested. `main` just runs the
//! app and turns any error into a friendly message plus a non-zero exit code.

use std::process::ExitCode;

fn main() -> ExitCode {
    // `shopli::run()` returns `anyhow::Result<()>`. We handle the two cases
    // ourselves so we control exactly how errors are printed.
    match shopli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // `{err:#}` prints the whole error chain: the top-level message
            // plus every `.context(...)` we attached along the way.
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
