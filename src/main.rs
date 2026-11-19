//! Binary entry point.
//!
//! Deliberately thin: parse, run, map errors to exit codes. Everything that could
//! be tested lives in the library.

use std::process::ExitCode;

fn main() -> ExitCode {
    if !sorta::platform::is_supported() {
        eprintln!(
            "sorta: unsupported platform '{}'; Sorta targets Windows 10/11 and Linux x86_64",
            std::env::consts::OS
        );
        return ExitCode::from(2);
    }

    // Sorting is background work; ask the OS to schedule it as such.
    let _ = sorta::platform::set_background_priority();

    match sorta::cli::run() {
        Ok(0) => ExitCode::SUCCESS,
        Ok(code) => ExitCode::from(code.clamp(1, 255) as u8),
        Err(err) => {
            eprintln!("sorta: {err:#}");
            ExitCode::from(1)
        }
    }
}
