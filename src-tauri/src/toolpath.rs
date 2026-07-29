//! Locating external tool binaries (`qemu-system-*`, `diskutil`,
//! `udisksctl`, …) from inside a GUI application.
//!
//! Two things make this harder than `Command::new("qemu-system-x86_64")`:
//!
//! 1. **A Finder-launched `.app` does not inherit your shell's `PATH`.**
//!    launchd hands it a minimal `/usr/bin:/bin:/usr/sbin:/sbin`, so every
//!    Homebrew tool is invisible — including the QEMU the app's own error
//!    message tells you to `brew install`. Running the binary from a terminal
//!    works, which is exactly why this looks like a phantom bug.
//!
//! 2. **`<bin> --version` is not an existence test.** `diskutil` is
//!    verb-based and answers `did not recognize verb "--version"` with exit 1,
//!    so probing that way reports a macOS built-in as missing.
//!
//! So resolution is separated from version-probing: [`resolve`] answers "where
//! is it", by *looking* rather than executing, and returns an absolute path.
//! Callers spawn that path, so detection and execution can never disagree —
//! the previous code detected by bare name and spawned by bare name, which
//! meant a passing check still gave you a failing spawn.
//!
//! Search order is the user's login-shell `PATH` first (what they'd get in a
//! terminal, and the most faithful reading of their intent), then the
//! inherited process `PATH`, then well-known install prefixes. The list is a
//! union, so a slow or broken login shell degrades to the fallbacks instead of
//! losing tools.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use crate::proc;

/// A login shell sources the user's rc files, which on a developer machine can
/// mean version managers, completions, and network-touching prompts. Two
/// seconds is generous for `printf $PATH` and still well inside the
/// "diagnostics complete in under a second" budget on the cached path.
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(2);

/// Install prefixes to search when `PATH` is the minimal one launchd provides.
/// Ordered most- to least-likely so the common case exits early.
#[cfg(target_os = "macos")]
const FALLBACK_DIRS: &[&str] = &[
    "/opt/homebrew/bin", // Homebrew, Apple Silicon
    "/usr/local/bin",    // Homebrew, Intel
    "/opt/local/bin",    // MacPorts
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
];

#[cfg(target_os = "linux")]
const FALLBACK_DIRS: &[&str] = &[
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
    "/home/linuxbrew/.linuxbrew/bin",
];

#[cfg(target_os = "windows")]
const FALLBACK_DIRS: &[&str] = &[];

/// The user's `PATH` as a login shell would see it.
///
/// Uses `-lic`: `-l` sources the profile, and `-i` is needed because on macOS
/// zsh reads `~/.zshrc` — where most people actually set `PATH` — only for
/// interactive shells. stdin is closed and the call is bounded, so an rc file
/// that tries to prompt fails fast rather than wedging startup.
///
/// `None` when there is no `$SHELL`, the shell errors, or it outruns the
/// timeout. Callers fall back to the process `PATH` plus [`FALLBACK_DIRS`].
#[cfg(unix)]
fn login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").ok()?;
    if shell.is_empty() {
        return None;
    }
    let mut cmd = Command::new(&shell);
    cmd.arg("-lic").arg("printf %s \"$PATH\"");
    let out = proc::output_with_timeout(cmd, LOGIN_SHELL_TIMEOUT).ok()?;
    if !out.status.success() {
        return None;
    }
    // An rc file that prints a banner pollutes stdout ahead of our printf, so
    // take the last non-empty line rather than the whole buffer.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let path = stdout.lines().rfind(|l| l.contains('/'))?.trim();
    (!path.is_empty()).then(|| path.to_string())
}

#[cfg(not(unix))]
fn login_shell_path() -> Option<String> {
    None
}

/// Directories to search, in priority order, deduplicated.
///
/// Computed once: the login-shell probe is far too expensive to repeat per
/// lookup, and `doctor` alone resolves several binaries per run.
fn search_dirs() -> &'static [PathBuf] {
    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut push = |p: PathBuf| {
            if !p.as_os_str().is_empty() && !dirs.contains(&p) {
                dirs.push(p);
            }
        };

        if let Some(login) = login_shell_path() {
            for p in std::env::split_paths(&OsString::from(login)) {
                push(p);
            }
        }
        if let Some(env_path) = std::env::var_os("PATH") {
            for p in std::env::split_paths(&env_path) {
                push(p);
            }
        }
        for d in FALLBACK_DIRS {
            push(PathBuf::from(d));
        }
        dirs
    })
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Absolute path to `bin`, or `None` if it isn't installed anywhere we look.
///
/// Does not execute anything — a binary that refuses `--version`, prints a
/// banner, or blocks on stdin still resolves correctly.
pub fn resolve(bin: &str) -> Option<PathBuf> {
    // An explicit path is used as given, so a configured absolute path isn't
    // silently overridden by something earlier on PATH.
    if bin.contains('/') || bin.contains('\\') {
        let p = PathBuf::from(bin);
        return is_executable(&p).then_some(p);
    }

    for dir in search_dirs() {
        let candidate = dir.join(bin);
        if is_executable(&candidate) {
            return Some(candidate);
        }
        // Windows records the extension in the filename; callers ask for the
        // bare stem so the same call site works on every platform.
        #[cfg(target_os = "windows")]
        for ext in ["exe", "cmd", "bat"] {
            let c = dir.join(format!("{bin}.{ext}"));
            if is_executable(&c) {
                return Some(c);
            }
        }
    }
    None
}

/// Whether `bin` is installed. Existence only — see [`resolve`].
pub fn exists(bin: &str) -> bool {
    resolve(bin).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_finds_a_universal_binary() {
        // `sh` is on every unix in a standard location.
        #[cfg(unix)]
        assert!(resolve("sh").is_some(), "sh should resolve on any unix");
    }

    #[test]
    fn resolve_returns_none_for_a_missing_binary() {
        assert!(resolve("disk-cutter-no-such-binary-abc123").is_none());
    }

    #[test]
    fn resolved_paths_are_absolute_and_executable() {
        #[cfg(unix)]
        {
            let p = resolve("sh").expect("sh resolves");
            assert!(p.is_absolute(), "callers spawn this path directly");
            assert!(is_executable(&p));
        }
    }

    /// The bug this module exists for: `diskutil` is a macOS built-in, but it
    /// has no `--version` verb and exits non-zero when handed one. Resolving
    /// by execution reported it missing and blamed the user's PATH.
    #[cfg(target_os = "macos")]
    #[test]
    fn diskutil_resolves_even_though_it_rejects_version_flag() {
        assert!(
            exists("diskutil"),
            "diskutil is a macOS built-in and must always resolve"
        );

        let rejects_version_flag = Command::new("diskutil")
            .arg("--version")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(false);
        assert!(
            rejects_version_flag,
            "if diskutil ever gains --version this test is obsolete, but \
             resolution must still not depend on it"
        );
    }

    #[test]
    fn an_explicit_path_is_honoured() {
        #[cfg(unix)]
        {
            assert_eq!(resolve("/bin/sh"), Some(PathBuf::from("/bin/sh")));
            assert!(resolve("/bin/definitely-not-here-xyz").is_none());
        }
    }

    #[test]
    fn search_dirs_are_deduplicated() {
        let dirs = search_dirs();
        let mut seen = std::collections::HashSet::new();
        for d in dirs {
            assert!(seen.insert(d), "duplicate search dir: {}", d.display());
        }
    }

    #[test]
    fn search_dirs_include_the_fallbacks() {
        let dirs = search_dirs();
        for d in FALLBACK_DIRS {
            let want = PathBuf::from(d);
            assert!(
                dirs.contains(&want),
                "fallback {d} missing — a minimal launchd PATH would lose it"
            );
        }
    }
}
