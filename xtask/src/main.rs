//! Project automation, invoked as `cargo xtask <task>`.
//!
//! * `ci` runs every quality gate a change must pass before it is committed:
//!   formatting, clippy (warnings are errors), tests, documentation, dependency
//!   audit (when `cargo-deny` is installed) and the content-free check.
//! * `content-tests` runs the `#[ignore]`d tests that need a real game install.
//! * `check-assets` verifies that no Valve content is tracked by git.
//!
//! Implemented with the standard library only so it builds instantly and
//! cannot be broken by a dependency.

// A task runner reports progress on the terminal by design.
#![allow(clippy::print_stdout, clippy::print_stderr)]
#![forbid(unsafe_code)]

use std::env;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// File extensions of Source engine content formats. None of these may ever be
/// committed: rustlife loads them from the player's own install.
const FORBIDDEN_EXTENSIONS: &[&str] = &[
    "bsp", "vpk", "vtf", "vmt", "mdl", "vvd", "vtx", "phy", "ani", "wav", "mp3", "pcf", "nav",
];

/// Magic numbers of Source content formats, to catch renamed files.
const FORBIDDEN_MAGIC: &[(&[u8], &str)] = &[
    (b"VBSP", "BSP map"),
    (b"VTF\0", "VTF texture"),
    (b"IDST", "MDL model"),
    (b"IDSV", "VVD vertex data"),
    (&[0x34, 0x12, 0xaa, 0x55], "VPK archive"),
];

fn main() -> ExitCode {
    let task = env::args().nth(1).unwrap_or_default();
    let result = match task.as_str() {
        "ci" => ci(),
        "content-tests" => content_tests(),
        "check-assets" => check_assets(),
        _ => {
            eprintln!("usage: cargo xtask <ci | content-tests | check-assets>");
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::FAILURE
        }
    }
}

type TaskResult = Result<(), String>;

fn ci() -> TaskResult {
    cargo(&["fmt", "--all", "--", "--check"], &[])?;
    cargo(
        &["clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"],
        &[],
    )?;
    cargo(&["test", "--workspace", "--all-features"], &[])?;
    cargo(&["doc", "--workspace", "--no-deps", "--all-features"], &[("RUSTDOCFLAGS", "-D warnings")])?;
    if has_cargo_subcommand("deny") {
        cargo(&["deny", "check"], &[])?;
    } else {
        eprintln!("xtask: cargo-deny not installed; skipping dependency audit (cargo install cargo-deny)");
    }
    check_assets()?;
    println!("xtask: all CI checks passed");
    Ok(())
}

fn content_tests() -> TaskResult {
    cargo(&["test", "--workspace", "--all-features", "--", "--ignored"], &[])
}

/// Fails if any tracked or staged file has a forbidden extension or content
/// signature.
fn check_assets() -> TaskResult {
    let root = workspace_root()?;
    let output = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard", "-z"])
        .current_dir(&root)
        .output()
        .map_err(|e| format!("failed to run git: {e}"))?;
    if !output.status.success() {
        return Err("git ls-files failed".into());
    }

    let mut violations = Vec::new();
    for name in output.stdout.split(|&b| b == 0).filter(|n| !n.is_empty()) {
        let relative = String::from_utf8_lossy(name);
        let path = root.join(relative.as_ref());
        let extension = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
        if extension.as_deref().is_some_and(|e| FORBIDDEN_EXTENSIONS.contains(&e)) {
            violations.push(format!("{relative}: forbidden content extension"));
        } else if let Some(kind) = sniff_magic(&path) {
            violations.push(format!("{relative}: looks like a {kind}"));
        }
    }

    if violations.is_empty() {
        println!("xtask: no game content found in the repository");
        Ok(())
    } else {
        Err(format!(
            "game content must never be committed:\n  {}",
            violations.join("\n  ")
        ))
    }
}

fn sniff_magic(path: &Path) -> Option<&'static str> {
    let mut header = [0u8; 4];
    File::open(path).ok()?.read_exact(&mut header).ok()?;
    FORBIDDEN_MAGIC.iter().find(|(magic, _)| header == **magic).map(|(_, kind)| *kind)
}

fn cargo(args: &[&str], env: &[(&str, &str)]) -> TaskResult {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    println!("xtask: cargo {}", args.join(" "));
    let status = Command::new(cargo)
        .args(args)
        .envs(env.iter().copied())
        .current_dir(workspace_root()?)
        .status()
        .map_err(|e| format!("failed to run cargo: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("`cargo {}` failed", args.join(" "))) }
}

fn has_cargo_subcommand(name: &str) -> bool {
    Command::new("cargo")
        .args([name, "--version"])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn workspace_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "xtask must live one level below the workspace root".into())
}
