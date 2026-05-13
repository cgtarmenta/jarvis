//! `jarvis` CLI entry point.
//!
//! Everything heavy lives in the library; `main` is intentionally small so
//! `cargo run -- ...` and integration tests share the same wiring.

use std::process::ExitCode;

fn main() -> ExitCode {
    match jarvis::cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Use the alternate formatter so anyhow's error chain prints,
            // not just the outermost message.
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}
