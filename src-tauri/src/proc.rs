//! Running external tools without ever blocking the main thread.
//!
//! Two hazards motivate this module:
//!
//!  1. A wedged device can make `diskutil` (and friends) hang indefinitely.
//!     Tauri runs synchronous `#[command] fn`s on the **main thread**, so a
//!     hung tool there freezes the whole UI — a blank, unresponsive window.
//!     The fix has two parts that must BOTH hold: commands that shell out are
//!     `async fn` and offload the blocking work via
//!     `tauri::async_runtime::spawn_blocking` (keeps the main thread free), and
//!     the spawn itself runs under [`output_with_timeout`] (so a hung tool
//!     returns an error instead of pinning a blocking-pool thread forever).
//!
//!  2. A child that writes more than the pipe buffer holds will block on
//!     `write` if we don't drain its output. We read stdout/stderr on
//!     dedicated threads so the timeout/kill path can never deadlock.

use std::io;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Run `cmd` to completion, killing it if it exceeds `timeout`.
///
/// Returns `io::ErrorKind::TimedOut` when the deadline elapses; the child is
/// killed and reaped before returning so we don't leak zombies. stdout/stderr
/// are drained concurrently, so this is safe for tools with large output.
///
/// Intended to be called from inside `spawn_blocking` — it sleeps on the
/// calling thread, which must never be the main thread.
pub fn output_with_timeout(mut cmd: Command, timeout: Duration) -> io::Result<Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;

    // Drain both pipes on their own threads so a chatty child can't fill the
    // pipe buffer and block before exiting (which would look like a hang).
    let mut stdout = child.stdout.take().expect("stdout piped above");
    let mut stderr = child.stderr.take().expect("stderr piped above");
    let (tx_out, rx_out) = mpsc::channel();
    let (tx_err, rx_err) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = io::Read::read_to_end(&mut stdout, &mut buf);
        let _ = tx_out.send(buf);
    });
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = io::Read::read_to_end(&mut stderr, &mut buf);
        let _ = tx_err.send(buf);
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("command timed out after {timeout:?}"),
                    ));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    };

    let stdout = rx_out.recv().unwrap_or_default();
    let stderr = rx_err.recv().unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

// These tests drive real child processes via unix shell tools
// (echo/sleep/sh/yes), so they only run on unix. The helper itself is
// cross-platform std::process and compiles everywhere.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn returns_output_for_fast_command() {
        let mut c = Command::new("echo");
        c.arg("hello");
        let out = output_with_timeout(c, Duration::from_secs(5)).expect("echo runs");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "hello");
    }

    #[test]
    fn times_out_on_hung_command() {
        let mut c = Command::new("sleep");
        c.arg("10");
        let start = Instant::now();
        let err = output_with_timeout(c, Duration::from_millis(200)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        // Should return promptly after the timeout, not after the full sleep.
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "killed near deadline"
        );
    }

    #[test]
    fn drains_large_output_without_deadlock() {
        // ~1 MiB of output, far exceeding the OS pipe buffer.
        let mut c = Command::new("sh");
        c.args(["-c", "yes 0123456789 | head -n 100000"]);
        let out = output_with_timeout(c, Duration::from_secs(10)).expect("completes");
        assert!(out.status.success());
        assert!(out.stdout.len() > 1_000_000);
    }
}
