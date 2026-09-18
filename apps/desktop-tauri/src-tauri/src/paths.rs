//! Development-mode filesystem roots for the Tauri desktop shell prototype.
//!
//! The prototype has no packaged layout yet: every path resolves inside the
//! repository checkout and the shared `apps/desktop` development build tree,
//! exactly the inputs the Electron development launcher prepares.

use std::env;
use std::path::{Path, PathBuf};

/// Resolved shell-owned process and document roots.
#[derive(Debug, Clone)]
pub struct ShellPaths {
    /// Node executable that runs the Desktop Host (system Node in development).
    pub node: String,
    /// Directory of private shell launchers injected into package-install PATH.
    pub node_bin: PathBuf,
    /// Bundled pnpm entry for profile package operations.
    pub pnpm: PathBuf,
    /// Development project carrying the linked dsh runtime packages.
    pub dsh_dir: PathBuf,
    /// Isolated Harness home for the Tauri shell (separate from Electron dev).
    pub home: PathBuf,
    /// Desktop profile directory the Host boots.
    pub profile: PathBuf,
    /// Bundled primary runtime payload (Python/Node interpreters).
    pub primary_runtime: PathBuf,
    /// Packaged Web client static document root.
    pub dist: PathBuf,
}

fn repository_root() -> Result<PathBuf, String> {
    if let Some(root) = env::var_os("DSH_TAURI_REPO_ROOT") {
        let root = PathBuf::from(root);
        if root.join("apps").join("desktop-tauri").is_dir() {
            return Ok(root);
        }
        return Err(format!(
            "dsh desktop tauri: DSH_TAURI_REPO_ROOT does not contain apps/desktop-tauri: {}",
            root.display()
        ));
    }
    // Compiled-in manifest directory of an unpackaged dev build: .../apps/desktop-tauri/src-tauri.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut candidate = manifest.parent().and_then(Path::parent);
    while let Some(dir) = candidate {
        if dir.join("apps").join("desktop-tauri").is_dir() {
            return Ok(dir.to_path_buf());
        }
        candidate = dir.parent();
    }
    Err(String::from(
        "dsh desktop tauri: cannot locate the repository root; set DSH_TAURI_REPO_ROOT",
    ))
}

fn desktop_build_target() -> Result<&'static str, String> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => Ok("mac-arm64"),
        ("macos", "x86_64") => Ok("mac-x64"),
        ("windows", "x86_64") => Ok("win-x64"),
        (os, arch) => Err(format!(
            "dsh desktop tauri: unsupported development platform {os}-{arch}"
        )),
    }
}

/// Resolve the Node executable to its absolute path.
///
/// The Desktop Host inherits a PATH whose `node` is the private
/// `node-bin` launcher that execs `$DSH_DESKTOP_NODE_EXECUTABLE`; a
/// relative value there would re-enter the launcher and recurse.
fn resolve_node() -> Result<String, String> {
    let candidate = env::var("DSH_TAURI_NODE").unwrap_or_else(|_| String::from("node"));
    let output = std::process::Command::new(&candidate)
        .arg("-p")
        .arg("process.execPath")
        .output()
        .map_err(|e| format!("running Node `{candidate}` from PATH: {e}; set DSH_TAURI_NODE"))?;
    let resolved = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || resolved.is_empty() {
        return Err(format!("resolving the Node executable via `{candidate}` failed"));
    }
    Ok(resolved)
}

impl ShellPaths {
    /// Resolve development roots and verify the prepared artifacts exist.
    pub fn resolve() -> Result<Self, String> {
        let repo = repository_root()?;
        let desktop_build = repo.join("apps").join("desktop").join(".desktop-build");
        let dsh_dir = desktop_build.join("development").join("project");
        let dist = dsh_dir
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh-web-frontend")
            .join("dist");
        let primary_runtime = desktop_build
            .join("targets")
            .join(desktop_build_target()?)
            .join("runtime")
            .join("primary-runtime");
        for (subject, path) in [
            ("development project", &dsh_dir),
            ("Web client dist", &dist),
            ("primary runtime", &primary_runtime),
        ] {
            if !path.is_dir() {
                return Err(format!(
                    "dsh desktop tauri: missing {} at {}; run `pnpm run dev:desktop-tauri` to prepare it",
                    subject,
                    path.display()
                ));
            }
        }
        Ok(Self {
            node: resolve_node()?,
            node_bin: repo.join("apps").join("desktop").join("scripts").join("node-bin"),
            pnpm: repo
                .join("apps")
                .join("desktop")
                .join("node_modules")
                .join("pnpm")
                .join("bin")
                .join("pnpm.mjs"),
            profile: desktop_build.join("development").join("home-tauri").join("profiles").join("desktop"),
            home: desktop_build.join("development").join("home-tauri"),
            dsh_dir,
            primary_runtime,
            dist,
        })
    }
}
