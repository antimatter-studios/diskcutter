//! Per-job structured logger.
//!
//! Every action tied to a queue item — decoder-chain probes, burn lifecycle,
//! verify mismatches — writes log lines into that item's `burn_logs` row in
//! the DB. Clicking the job in the queue surfaces the log inline, so the
//! forensic record sits next to the data instead of being hunted out of a
//! global stream.
//!
//! Levels mirror the standard log crates (debug / info / warn / error).
//! `debug` is gated by the `debug.logging` preference; when off it's a
//! cheap no-op (no string formatting cost if the caller uses the
//! `log.debug_enabled()` guard before constructing a message).
//!
//! Two implementations:
//! - `DbJobLogger` — used by the parent process for in-process burns and
//!   for parent-side dispatch of helper-emitted log lines.
//! - `NullLogger` — drops everything; for code paths not tied to a job
//!   (probe/inspect/CLI).
//!
//! Helper subprocess uses its own implementation (in `helper.rs`) that
//! emits log lines as JSONL `HelperMessage::Log` records the parent's
//! `tail_helper` thread parses and forwards to `DbJobLogger`.

use tauri::{AppHandle, Manager};

use crate::db::{self, Db};

/// Look up the user's `debug.logging` preference. Returns false if the DB
/// isn't managed (early-startup paths) or the key is unset.
pub fn debug_logging_enabled(app: &AppHandle) -> bool {
    let Some(db) = app.try_state::<Db>() else {
        return false;
    };
    let Ok(conn) = db.0.lock() else {
        return false;
    };
    conn.query_row(
        "SELECT value FROM config WHERE key = 'debug.logging'",
        [],
        |r| r.get::<_, String>(0),
    )
    .map(|v| v == "true")
    .unwrap_or(false)
}

/// Build a `DbJobLogger` for `job_id` with the current debug-logging pref
/// snapshot. Use this at the entry point of any scan/burn worker so the
/// pref value is captured once for the duration of the work (no mid-scan
/// toggling).
pub fn db_logger_for(app: &AppHandle, job_id: i64) -> DbJobLogger {
    DbJobLogger::new(app.clone(), job_id, debug_logging_enabled(app))
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "debug" => Self::Debug,
            "warn" => Self::Warn,
            "error" => Self::Error,
            _ => Self::Info,
        }
    }
}

pub trait JobLogger: Send + Sync {
    fn log(&self, level: LogLevel, message: &str);

    /// Returns whether `debug` will actually be recorded. Callers building
    /// expensive debug messages should guard with this to avoid the format
    /// cost when the user has the toggle off.
    fn debug_enabled(&self) -> bool {
        true
    }

    fn debug(&self, message: &str) {
        if self.debug_enabled() {
            self.log(LogLevel::Debug, message);
        }
    }
    fn info(&self, message: &str) {
        self.log(LogLevel::Info, message);
    }
    fn warn(&self, message: &str) {
        self.log(LogLevel::Warn, message);
    }
    fn error(&self, message: &str) {
        self.log(LogLevel::Error, message);
    }
}

/// Drops every log call. For probe / inspect paths that aren't attached to a
/// queue item — there is no `burn_logs` row to write to, and emitting to
/// stderr would just produce noise nobody reads.
pub struct NullLogger;

impl JobLogger for NullLogger {
    fn log(&self, _: LogLevel, _: &str) {}
    fn debug_enabled(&self) -> bool {
        false
    }
}

/// Writes log lines into the `burn_logs` row for `job_id` via the parent
/// app's `Db` state. Cloned across the burn thread by capturing the
/// `AppHandle`; the `Db` is resolved per log call (cheap HashMap lookup,
/// no need to outlive the spawning function).
///
/// `debug_enabled` is snapshot at construction so toggling the pref
/// mid-burn doesn't half-apply (matches the existing burn_params
/// snapshot pattern).
pub struct DbJobLogger {
    app: AppHandle,
    job_id: i64,
    debug_enabled: bool,
}

impl DbJobLogger {
    pub fn new(app: AppHandle, job_id: i64, debug_enabled: bool) -> Self {
        Self {
            app,
            job_id,
            debug_enabled,
        }
    }
}

impl JobLogger for DbJobLogger {
    fn log(&self, level: LogLevel, message: &str) {
        if level == LogLevel::Debug && !self.debug_enabled {
            return;
        }
        if let Some(db) = self.app.try_state::<Db>() {
            db::append_log(&db, self.job_id, level.as_str(), message);
        }
    }

    fn debug_enabled(&self) -> bool {
        self.debug_enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    /// Test logger that records every call for assertion.
    pub struct RecordingLogger {
        pub entries: StdMutex<Vec<(LogLevel, String)>>,
        pub debug_enabled: bool,
    }

    impl RecordingLogger {
        pub fn new(debug_enabled: bool) -> Self {
            Self {
                entries: StdMutex::new(Vec::new()),
                debug_enabled,
            }
        }
        pub fn entries(&self) -> Vec<(LogLevel, String)> {
            self.entries.lock().unwrap().clone()
        }
    }

    impl JobLogger for RecordingLogger {
        fn log(&self, level: LogLevel, message: &str) {
            if level == LogLevel::Debug && !self.debug_enabled {
                return;
            }
            self.entries
                .lock()
                .unwrap()
                .push((level, message.to_string()));
        }
        fn debug_enabled(&self) -> bool {
            self.debug_enabled
        }
    }

    #[test]
    fn null_logger_drops_everything() {
        let l = NullLogger;
        l.debug("d");
        l.info("i");
        l.warn("w");
        l.error("e");
        assert!(!l.debug_enabled());
    }

    #[test]
    fn level_round_trips_through_strings() {
        for lvl in [
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error,
        ] {
            assert_eq!(LogLevel::parse(lvl.as_str()), lvl);
        }
    }

    #[test]
    fn level_from_str_falls_back_to_info_for_unknown() {
        assert_eq!(LogLevel::parse("nope"), LogLevel::Info);
        assert_eq!(LogLevel::parse(""), LogLevel::Info);
    }

    #[test]
    fn recording_logger_with_debug_off_drops_debug_keeps_others() {
        let l = RecordingLogger::new(false);
        l.debug("d");
        l.info("i");
        l.warn("w");
        l.error("e");
        let levels: Vec<LogLevel> = l.entries().iter().map(|(lv, _)| *lv).collect();
        assert_eq!(
            levels,
            vec![LogLevel::Info, LogLevel::Warn, LogLevel::Error]
        );
    }

    #[test]
    fn recording_logger_with_debug_on_keeps_debug() {
        let l = RecordingLogger::new(true);
        l.debug("d");
        l.info("i");
        let levels: Vec<LogLevel> = l.entries().iter().map(|(lv, _)| *lv).collect();
        assert_eq!(levels, vec![LogLevel::Debug, LogLevel::Info]);
    }
}
