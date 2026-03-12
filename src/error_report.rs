use std::backtrace::BacktraceStatus;

use anyhow::Error;

pub fn print_cli_error(err: &Error) {
    eprintln!("\nUnhandled JWC error:");
    eprintln!("  Message: {}", err);

    let mut causes = err.chain();
    let _ = causes.next();
    for (idx, cause) in causes.enumerate() {
        eprintln!("  Caused by[{idx}]: {cause}");
    }

    let bt = err.backtrace();
    if bt.status() == BacktraceStatus::Captured {
        eprintln!("\nBacktrace:\n{bt}");
    } else {
        eprintln!("  Tip: set RUST_BACKTRACE=1 to include backtrace details.");
    }
}

pub fn log_runtime_error(context: &str, err: &Error) {
    eprintln!("[JWC-ERROR] {context}");
    eprintln!("[JWC-ERROR] Message: {err}");

    let mut causes = err.chain();
    let _ = causes.next();
    for (idx, cause) in causes.enumerate() {
        eprintln!("[JWC-ERROR] Caused by[{idx}]: {cause}");
    }
}

pub fn to_single_line(err: &Error) -> String {
    let mut parts = Vec::new();
    for cause in err.chain() {
        parts.push(cause.to_string());
    }
    parts.join(" | caused by: ")
}
