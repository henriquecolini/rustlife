//! Project automation, invoked as `cargo xtask <task>`.
//!
//! * `ci` runs every quality gate a change must pass before it is committed:
//!   formatting, clippy (warnings are errors), tests, documentation, dependency
//!   audit (when `cargo-deny` is installed), a type-check for the browser
//!   target (when it is installed) and the content-free check.
//! * `web-test` runs the browser tests of `rl_web` in headless Chrome (needs
//!   `wasm-bindgen-test-runner` and a matching `chromedriver`); `ci` runs it
//!   too when both are installed.
//! * `web-build` builds the browser version into `dist/web` (needs
//!   `wasm-bindgen`, at the version in `Cargo.lock`), and `web-serve` serves
//!   that folder on localhost.
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
        "web-test" => web_test(),
        "web-build" => web_build(),
        "web-serve" => web_serve(),
        "content-tests" => content_tests(),
        "check-assets" => check_assets(),
        _ => {
            eprintln!(
                "usage: cargo xtask <ci | web-test | web-build | web-serve | content-tests | check-assets>"
            );
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

/// The browser target the web build is compiled for.
const WASM_TARGET: &str = "wasm32-unknown-unknown";

/// Crates that must keep compiling for the browser. The headless server
/// binary, the inspector and `rustlife` itself are native-only.
const WASM_CRATES: &[&str] = &[
    "rl_assets",
    "rl_audio",
    "rl_client",
    "rl_config",
    "rl_data",
    "rl_level",
    "rl_net",
    "rl_server",
    "rl_sim",
    "rl_web",
    "source_formats",
    "source_fs",
];

fn ci() -> TaskResult {
    cargo(&["fmt", "--all", "--", "--check"], &[])?;
    cargo(
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
        &[],
    )?;
    cargo(&["test", "--workspace", "--all-features"], &[])?;
    cargo(
        &["doc", "--workspace", "--no-deps", "--all-features"],
        &[("RUSTDOCFLAGS", "-D warnings")],
    )?;
    if has_cargo_subcommand("deny") {
        cargo(&["deny", "check"], &[])?;
    } else {
        eprintln!(
            "xtask: cargo-deny not installed; skipping dependency audit (cargo install cargo-deny)"
        );
    }
    wasm_check()?;
    if has_browser_test_tools() {
        web_test()?;
    } else {
        eprintln!(
            "xtask: wasm-bindgen-test-runner or chromedriver not installed; skipping the browser tests"
        );
    }
    check_assets()?;
    println!("xtask: all CI checks passed");
    Ok(())
}

/// Type-checks the web-capable crates for the browser, so native-only code
/// cannot creep into them unnoticed. Skipped, with a note, when the target is
/// not installed.
fn wasm_check() -> TaskResult {
    if !has_rust_target(WASM_TARGET) {
        eprintln!(
            "xtask: {WASM_TARGET} not installed; skipping the browser check \
             (rustup target add {WASM_TARGET})"
        );
        return Ok(());
    }
    let mut args = vec!["check", "--target", WASM_TARGET];
    for name in WASM_CRATES {
        args.extend(["-p", name]);
    }
    cargo(&args, &[])?;
    // Code that only exists for the browser is not seen by the ordinary
    // clippy run, so lint it for its own target.
    cargo(
        &[
            "clippy",
            "--target",
            WASM_TARGET,
            "-p",
            "rl_web",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        &[],
    )
}

/// Runs the browser tests of the web build in headless Chrome.
fn web_test() -> TaskResult {
    if !has_rust_target(WASM_TARGET) {
        return Err(format!(
            "{WASM_TARGET} is not installed (rustup target add {WASM_TARGET})"
        ));
    }
    if !has_browser_test_tools() {
        return Err(
            "the browser tests need `wasm-bindgen-test-runner` (cargo binstall wasm-bindgen-cli, \
             at the version in Cargo.lock) and a `chromedriver` matching Chrome on the PATH"
                .into(),
        );
    }
    cargo(
        &[
            "test",
            "--target",
            WASM_TARGET,
            "-p",
            "rl_web",
            "--test",
            "browser",
        ],
        &[],
    )
}

/// The cargo profile the browser build uses (see the workspace `Cargo.toml`).
const WEB_PROFILE: &str = "web";

/// Builds the browser version into `dist/web`: the wasm module and its
/// JavaScript bindings under `pkg/`, and the page around them.
fn web_build() -> TaskResult {
    if !has_rust_target(WASM_TARGET) {
        return Err(format!(
            "{WASM_TARGET} is not installed (rustup target add {WASM_TARGET})"
        ));
    }
    let root = workspace_root()?;
    let wanted = locked_version("wasm-bindgen")?;
    let found = Command::new("wasm-bindgen")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .ok_or("`wasm-bindgen` is not installed (cargo binstall wasm-bindgen-cli)")?;
    if found.split_whitespace().nth(1) != Some(wanted.as_str()) {
        return Err(format!(
            "wasm-bindgen is {}, but Cargo.lock needs {wanted}: cargo binstall wasm-bindgen-cli@{wanted}",
            found.trim()
        ));
    }
    cargo(
        &[
            "build",
            "--target",
            WASM_TARGET,
            "--profile",
            WEB_PROFILE,
            "-p",
            "rl_web",
        ],
        &[],
    )?;
    let dist = root.join("dist/web");
    let wasm = root
        .join("target")
        .join(WASM_TARGET)
        .join(WEB_PROFILE)
        .join("rl_web.wasm");
    let status = Command::new("wasm-bindgen")
        .args(["--target", "web", "--no-typescript", "--out-dir"])
        .arg(dist.join("pkg"))
        .arg(&wasm)
        .status()
        .map_err(|e| format!("failed to run wasm-bindgen: {e}"))?;
    if !status.success() {
        return Err("wasm-bindgen failed".into());
    }
    for entry in std::fs::read_dir(root.join("crates/rl_web/web")).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        std::fs::copy(entry.path(), dist.join(entry.file_name())).map_err(|e| e.to_string())?;
    }
    let size = std::fs::metadata(dist.join("pkg/rl_web_bg.wasm")).map_or(0, |m| m.len());
    println!(
        "xtask: built {} ({:.1} MB of wasm)",
        dist.display(),
        size as f64 / 1_048_576.0
    );
    Ok(())
}

/// The version of crate `name` that `Cargo.lock` pins.
fn locked_version(name: &str) -> Result<String, String> {
    let lock = std::fs::read_to_string(workspace_root()?.join("Cargo.lock"))
        .map_err(|e| format!("cannot read Cargo.lock: {e}"))?;
    let marker = format!("name = \"{name}\"\nversion = \"");
    let start = lock
        .find(&marker)
        .ok_or_else(|| format!("{name} is not in Cargo.lock"))?
        + marker.len();
    lock[start..]
        .split('"')
        .next()
        .map(str::to_owned)
        .ok_or_else(|| format!("cannot read {name}'s version from Cargo.lock"))
}

/// Serves `dist/web` on `http://localhost:8080` until stopped. Localhost is a
/// secure context, so WebGPU and the browser's file system work there.
fn web_serve() -> TaskResult {
    use std::io::Write;
    use std::net::TcpListener;

    let root = workspace_root()?.join("dist/web");
    if !root.is_dir() {
        return Err("dist/web does not exist: run `cargo xtask web-build` first".into());
    }
    let port = env::args().nth(2).unwrap_or_else(|| "8080".into());
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).map_err(|e| e.to_string())?;
    println!(
        "xtask: serving {} at http://localhost:{port}/ (Ctrl+C to stop)",
        root.display()
    );
    for stream in listener.incoming().flatten() {
        let root = root.clone();
        std::thread::spawn(move || {
            let mut stream = stream;
            let mut request = [0u8; 4096];
            let Ok(read) = Read::read(&mut stream, &mut request) else {
                return;
            };
            let head = String::from_utf8_lossy(&request[..read]);
            let target = head
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .split(['?', '#'])
                .next()
                .unwrap_or("/");
            let relative = target.trim_start_matches('/');
            let relative = if relative.is_empty() {
                "index.html"
            } else {
                relative
            };
            // Nothing outside the folder, whatever the request says.
            let safe = !relative
                .split('/')
                .any(|part| part == ".." || part.is_empty());
            let body = safe
                .then(|| std::fs::read(root.join(relative)).ok())
                .flatten();
            let (status, content_type, body) = match body {
                Some(body) => ("200 OK", content_type(relative), body),
                None => ("404 Not Found", "text/plain", b"not found".to_vec()),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
                 Cache-Control: no-store\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        });
    }
    Ok(())
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "css" => "text/css; charset=utf-8",
        "json" | "webmanifest" => "application/json",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn has_browser_test_tools() -> bool {
    ["wasm-bindgen-test-runner", "chromedriver"]
        .iter()
        .all(|tool| {
            Command::new(tool)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        })
}

fn content_tests() -> TaskResult {
    cargo(
        &["test", "--workspace", "--all-features", "--", "--ignored"],
        &[],
    )
}

/// Fails if any tracked or staged file has a forbidden extension or content
/// signature.
fn check_assets() -> TaskResult {
    let root = workspace_root()?;
    let output = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
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
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        if extension
            .as_deref()
            .is_some_and(|e| FORBIDDEN_EXTENSIONS.contains(&e))
        {
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
    FORBIDDEN_MAGIC
        .iter()
        .find(|(magic, _)| header == **magic)
        .map(|(_, kind)| *kind)
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
    if status.success() {
        Ok(())
    } else {
        Err(format!("`cargo {}` failed", args.join(" ")))
    }
}

fn has_cargo_subcommand(name: &str) -> bool {
    Command::new("cargo")
        .args([name, "--version"])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Whether the standard library for `target` is installed.
fn has_rust_target(target: &str) -> bool {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    Command::new(rustc)
        .args(["--print", "target-libdir", "--target", target])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|dir| Path::new(dir.trim()).is_dir())
}

fn workspace_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "xtask must live one level below the workspace root".into())
}
