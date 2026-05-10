use std::collections::BTreeMap as Map;
use std::env;
use std::path::Path;
use std::process::ExitCode;

use anyhow::Result;

use crate::{
    cli::{CargoVerusCli, VerusSubcommand},
    subcommands::{self, CargoRunPlan, NewCreationPlan, PostRunCommand, VerusConfig},
};

pub enum ExecutionPlan {
    CreateNew(NewCreationPlan),
    RunCargo(CargoRunPlan),
}

pub fn execute_plan(plan: &ExecutionPlan) -> Result<ExitCode> {
    use ExecutionPlan::*;

    match plan {
        CreateNew(creation_plan) => subcommands::create_new_project(creation_plan),
        RunCargo(cargo_run_plan) => subcommands::run_cargo(cargo_run_plan),
    }
}

pub fn plan_execution<'a>(
    current_dir: Option<&Path>,
    args: impl IntoIterator<Item = &'a str>,
) -> Result<ExecutionPlan> {
    let parsed_cli = CargoVerusCli::from_args(args.into_iter())?;

    let current_dir =
        if let Some(path) = current_dir { path.to_owned() } else { env::current_dir()? };

    let cfg = match parsed_cli.command {
        VerusSubcommand::New(new_cmd) => {
            let creation_plan = match (new_cmd.bin, new_cmd.lib) {
                (Some(name), None) => NewCreationPlan { current_dir, name, is_bin: true },
                (None, Some(name)) => NewCreationPlan { current_dir, name, is_bin: false },
                _ => unreachable!("clap enforces exactly one of --bin/--lib"),
            };
            return Ok(ExecutionPlan::CreateNew(creation_plan));
        }
        VerusSubcommand::Verify(options) => VerusConfig {
            current_dir,
            subcommand: "check",
            options,
            compile_primary: false,
            verify_deps: true,
            warn_if_nothing_verified: true,
            verify_anything: true,
            extra_env: Map::new(),
        },
        VerusSubcommand::Focus(options) => VerusConfig {
            current_dir,
            subcommand: "check",
            options,
            compile_primary: false,
            verify_deps: false,
            warn_if_nothing_verified: true,
            verify_anything: true,
            extra_env: Map::new(),
        },
        VerusSubcommand::Build(options) => VerusConfig {
            current_dir,
            subcommand: "build",
            options,
            compile_primary: true,
            verify_deps: true,
            warn_if_nothing_verified: false,
            verify_anything: true,
            extra_env: Map::new(),
        },
        VerusSubcommand::Check(options) => VerusConfig {
            current_dir,
            subcommand: "check",
            options,
            compile_primary: false,
            verify_deps: true,
            warn_if_nothing_verified: true,
            verify_anything: true,
            extra_env: Map::new(),
        },
        VerusSubcommand::Doc(mut options) => {
            ensure_no_deps_flag(&mut options.cargo_opts.cargo_args);
            VerusConfig {
                current_dir,
                subcommand: "doc",
                options,
                // The wrapped rustc build of the workspace + its verus
                // deps populates target/debug/deps with rmetas; rustdoc
                // then reads those rmetas when documenting workspace
                // crates. Use the same compile-on-pass-through behaviour
                // as `cargo verus build`.
                compile_primary: true,
                // Verify the whole tree by default (matches the
                // `cargo verus check` defaults). This also keeps vstd
                // on the standard verify path: vstd's source uses
                // `pub assume_specification<T, A: Allocator>[…]` syntax
                // that the driver only accepts when the package goes
                // through full verus-mode VIR translation. Forwarding
                // `--no-verify` for a doc-only run trips
                // `assume_specification declaration not supported here`
                // in `rust_to_vir_func`.
                verify_deps: true,
                verify_anything: true,
                warn_if_nothing_verified: false,
                extra_env: doc_extra_env(),
            }
        }
    };

    let mut cargo_run_plan = subcommands::plan_cargo_run(cfg)?;

    // After cargo doc finishes, run the verusdoc HTML post-processor on
    // the workspace's target/ directory. The post-processor finds the
    // `verusdoc_special_attr` markers the verus_!{} macro injected
    // (when VERUSDOC=1) and rewrites them into spec/proof/requires/
    // ensures sections in the rendered HTML.
    if matches!(plan_subcommand(&cargo_run_plan), Some("doc")) {
        let target_dir = workspace_target_dir(&cargo_run_plan);
        cargo_run_plan.post_run.push(PostRunCommand {
            program: verusdoc_path(),
            args: vec![],
            current_dir: target_dir,
            description: "verusdoc HTML post-processor".to_owned(),
        });
    }

    Ok(ExecutionPlan::RunCargo(cargo_run_plan))
}

/// `cargo verus doc` always passes `--no-deps` to cargo. Documenting
/// the verus runtime deps (vstd, verus_builtin, …) would re-process
/// their source under our `RUSTDOCFLAGS`, but those crates already
/// declare the same `register_tool(verus)` / `feature(stmt_expr_attributes)`
/// crate-attrs via `#![cfg_attr(verus_keep_ghost, ...)]`, so adding
/// the flags again triggers `tool already registered` errors. Users
/// who want to document deps can fall back to a plain `cargo doc`
/// invocation.
fn ensure_no_deps_flag(cargo_args: &mut Vec<String>) {
    if cargo_args.iter().any(|arg| arg == "--no-deps") {
        return;
    }
    cargo_args.push("--no-deps".to_owned());
}

/// Environment overrides for the doc subcommand. These ride alongside
/// the `RUSTC_WRAPPER` / `__VERUS_DRIVER_VIA_CARGO__` pair set by every
/// cargo-verus invocation.
///
/// - `VERUSDOC=1`: the `verus_!{}` macro reads this via
///   `proc_macro::tracked::env_var` and only injects the
///   `verusdoc_special_attr` doc markers when set.
/// - `RUSTC_BOOTSTRAP=1`: the doc build relies on nightly-only
///   features (`stmt_expr_attributes`, `register_tool`, …) that the
///   verus driver normally enables via `-Zcrate-attr` for crates it
///   wraps. The same flags appear in `RUSTDOCFLAGS` below for the
///   workspace crates that rustdoc compiles directly.
/// - `RUSTDOCFLAGS`: rustdoc's compile of workspace crates is *not*
///   wrapped by `RUSTC_WRAPPER`, so the cfg + feature plumbing the
///   driver injects for rustc-wrapped builds has to be replicated
///   here. The flag set mirrors
///   `rust_verify::config::enable_default_features_and_verus_attr` and
///   `extend_rustc_args_for_builtin_and_builtin_macros`.
fn doc_extra_env() -> Map<String, String> {
    let mut m = Map::new();
    m.insert("RUSTC_BOOTSTRAP".to_owned(), "1".to_owned());

    // Point cargo at our rustdoc shim. The shim sets `VERUSDOC=1`
    // before exec'ing the real rustdoc, which makes the verus_!{}
    // macro inject `verusdoc_special_attr` doc markers — but ONLY
    // during the rustdoc subprocess. The wrapped rustc invocations
    // that build dep rmetas (vstd etc.) don't see VERUSDOC=1, so
    // they don't try to pair marker injection with the
    // `assume_specification` shapes the VIR translation rejects.
    let shim = verus_rustdoc_shim_path();
    m.insert("RUSTDOC".to_owned(), shim.to_string_lossy().into_owned());
    // The shim looks up the real rustdoc from this env var first,
    // and falls back to a PATH lookup of `rustdoc` if unset. We try
    // to resolve it eagerly so unusual rustup setups still work.
    if let Some(real) = locate_real_rustdoc() {
        m.insert(
            "VERUS_REAL_RUSTDOC".to_owned(),
            real.to_string_lossy().into_owned(),
        );
    }

    // The flag set mirrors what
    // `rust_verify::config::enable_default_features_and_verus_attr`
    // injects for rustc-wrapped builds: workspace crates compiled by
    // rustdoc need the same nightly features and tool registrations,
    // since rustdoc isn't routed through the verus driver.
    let rustdocflags = [
        "--cfg=verus_keep_ghost",
        "--cfg=verus_keep_ghost_body",
        "--cfg=verus_only",
        "-Zcrate-attr=feature(stmt_expr_attributes)",
        "-Zcrate-attr=feature(box_patterns)",
        "-Zcrate-attr=feature(negative_impls)",
        "-Zcrate-attr=feature(rustc_attrs)",
        "-Zcrate-attr=feature(unboxed_closures)",
        "-Zcrate-attr=feature(register_tool)",
        "-Zcrate-attr=feature(tuple_trait)",
        "-Zcrate-attr=feature(custom_inner_attributes)",
        "-Zcrate-attr=feature(try_trait_v2)",
        "-Zcrate-attr=register_tool(verus)",
        "-Zcrate-attr=register_tool(verifier)",
        "-Zcrate-attr=register_tool(verusfmt)",
        "-Zcrate-attr=allow(internal_features)",
        "-Zcrate-attr=allow(unused_braces)",
        "-Zproc-macro-backtrace",
        "-Zunstable-options",
    ]
    .join(" ");
    m.insert("RUSTDOCFLAGS".to_owned(), rustdocflags);

    m
}

/// First arg of a `CargoRunPlan` is always the cargo subcommand
/// (`check` / `build` / `doc`). Returns it so callers can branch on
/// what cargo is being asked to do.
fn plan_subcommand(plan: &CargoRunPlan) -> Option<&str> {
    plan.args.first().map(String::as_str)
}

/// Resolve the workspace target directory from the cargo args the plan
/// will invoke. Falls back to `<current_dir>/target` if the plan does
/// not pass `--target-dir` explicitly.
fn workspace_target_dir(plan: &CargoRunPlan) -> std::path::PathBuf {
    let mut iter = plan.args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--target-dir" {
            if let Some(value) = iter.next() {
                return std::path::PathBuf::from(value);
            }
        } else if let Some(rest) = arg.strip_prefix("--target-dir=") {
            return std::path::PathBuf::from(rest);
        }
    }
    plan.current_dir.join("target")
}

/// Path to the `verusdoc` binary that ships alongside `cargo-verus` in
/// the verus distribution. Mirrors `get_verus_driver_path` in
/// `subcommands.rs`.
fn verusdoc_path() -> std::path::PathBuf {
    let mut path = env::current_exe()
        .expect("current executable path invalid")
        .with_file_name("verusdoc");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

/// Path to the `verus-rustdoc-shim` binary that ships alongside
/// `cargo-verus`. Same lookup pattern as `get_verus_driver_path` /
/// `verusdoc_path`: file-replacement on the cargo-verus binary path.
fn verus_rustdoc_shim_path() -> std::path::PathBuf {
    let mut path = env::current_exe()
        .expect("current executable path invalid")
        .with_file_name("verus-rustdoc-shim");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

/// Resolve the real rustdoc binary so the shim can `exec` it without
/// re-entering itself (which would loop, since the shim sets
/// `RUSTDOC=path/to/shim`).
///
/// Strategy:
///
/// 1. If `VERUS_REAL_RUSTDOC` is already set in our environment,
///    honour it (lets users / scripts pin the rustdoc).
/// 2. Otherwise, ask `rustup` to resolve `rustdoc` for the active
///    toolchain. This is the common case for users who installed
///    Rust via rustup.
/// 3. If rustup isn't available, fall back to PATH lookup of
///    `rustdoc` and let the shim do the same fallback at runtime.
fn locate_real_rustdoc() -> Option<std::path::PathBuf> {
    if let Some(p) = env::var_os("VERUS_REAL_RUSTDOC") {
        return Some(p.into());
    }
    if let Ok(out) = std::process::Command::new("rustup")
        .args(["which", "rustdoc"])
        .output()
    {
        if out.status.success() {
            let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if !path.is_empty() {
                return Some(std::path::PathBuf::from(path));
            }
        }
    }
    None
}
