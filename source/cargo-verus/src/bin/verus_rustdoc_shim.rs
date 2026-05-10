//! A tiny rustdoc wrapper that sets `VERUSDOC=1` in the environment
//! before exec'ing the real `rustdoc`.
//!
//! `cargo verus doc` points cargo's `RUSTDOC` env var at this binary
//! so the marker-injection trigger (`VERUSDOC=1`) reaches only the
//! rustdoc subprocess. The wrapped rustc invocations that build
//! workspace deps (vstd, verus_builtin, …) don't see it, which is
//! load-bearing: the verus_!{} macro pairs marker injection with
//! `assume_specification` shapes that the VIR translation in the
//! rustc-driven verify path rejects.
//!
//! The real rustdoc is located either via the `VERUS_REAL_RUSTDOC`
//! env var (set by `cargo verus doc` so this shim doesn't have to
//! shell out to `rustup which`) or, as a last resort, by `PATH`
//! lookup of `rustdoc`.

use std::process::{Command, ExitCode};

const REAL_RUSTDOC_ENV: &str = "VERUS_REAL_RUSTDOC";
const SELF_REENTRY_GUARD: &str = "VERUS_RUSTDOC_SHIM_ACTIVE";

fn main() -> ExitCode {
    if std::env::var_os(SELF_REENTRY_GUARD).is_some() {
        eprintln!(
            "verus-rustdoc-shim: refusing to re-enter (set {SELF_REENTRY_GUARD}). \
             Check that {REAL_RUSTDOC_ENV} or PATH points at the real rustdoc, not this shim."
        );
        return ExitCode::from(127);
    }

    let real_rustdoc = std::env::var_os(REAL_RUSTDOC_ENV)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("rustdoc"));

    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();

    if let Ok(log_path) = std::env::var("VERUS_RUSTDOC_SHIM_LOG") {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            use std::io::Write as _;
            let _ = writeln!(
                f,
                "shim invoked, real_rustdoc={}, args={:?}",
                real_rustdoc.display(),
                args
            );
        }
    }

    let status = match Command::new(&real_rustdoc)
        .args(&args)
        .env("VERUSDOC", "1")
        .env(SELF_REENTRY_GUARD, "1")
        .status()
    {
        Ok(s) => s,
        Err(err) => {
            eprintln!(
                "verus-rustdoc-shim: failed to exec {}: {err}",
                real_rustdoc.display()
            );
            return ExitCode::from(127);
        }
    };

    match status.code() {
        Some(code) => {
            // u8 truncation is fine here — process exit codes above 255
            // are not portable and cargo treats them the same as 1.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            ExitCode::from(code as u8)
        }
        None => ExitCode::from(128),
    }
}
