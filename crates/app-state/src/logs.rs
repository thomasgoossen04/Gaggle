//! In-memory `tracing` capture, feeding the GUI's Logs tab.
//!
//! [`init`] installs a process-global subscriber once (idempotent — later
//! calls just return the same handle) that keeps the most recent lines in a
//! bounded ring buffer, from every crate in the process (`net`, `app-state`,
//! `control-plane`, …), independent of whether a terminal is even attached —
//! the packaged GUI has none, which is exactly why a background-task warning
//! like a failed relay reservation was invisible before this existed.

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// How many of the most recent lines are kept; older ones are dropped.
const MAX_LINES: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn label(self) -> &'static str {
        match self {
            LogLevel::Trace => "TRACE",
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }

    fn from_tracing(level: tracing::Level) -> Self {
        match level {
            tracing::Level::TRACE => LogLevel::Trace,
            tracing::Level::DEBUG => LogLevel::Debug,
            tracing::Level::INFO => LogLevel::Info,
            tracing::Level::WARN => LogLevel::Warn,
            tracing::Level::ERROR => LogLevel::Error,
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One captured log line.
#[derive(Debug, Clone)]
pub struct LogLine {
    /// Seconds since the Unix epoch — cheap to format, no timezone dependency.
    pub time_unix: u64,
    pub level: LogLevel,
    /// The emitting module path, e.g. `net::node` or `app_state::manager`.
    pub target: String,
    pub message: String,
}

struct Inner {
    lines: Mutex<VecDeque<LogLine>>,
    /// Bumped on every captured line and on `clear()` — a cheap way for a
    /// poller to tell "did anything change?" without locking `lines` and
    /// cloning the buffer. Monotonic even once the ring buffer is full and
    /// `lines.len()` stops changing (push+evict keeps the length constant).
    version: AtomicU64,
}

/// A cheap-to-clone handle onto the process's captured log lines.
#[derive(Clone)]
pub struct LogHandle(Arc<Inner>);

impl LogHandle {
    /// Every currently-buffered line, oldest first.
    pub fn snapshot(&self) -> Vec<LogLine> {
        self.0.lines.lock().unwrap().iter().cloned().collect()
    }

    /// Monotonically increasing counter, bumped on every new line and on
    /// `clear()`. Compare against a previously-read value to skip a
    /// `snapshot()` (and the re-render it would trigger) when nothing new
    /// has been logged since.
    pub fn version(&self) -> u64 {
        self.0.version.load(Ordering::Relaxed)
    }

    pub fn clear(&self) {
        self.0.lines.lock().unwrap().clear();
        self.0.version.fetch_add(1, Ordering::Relaxed);
    }
}

struct CaptureLayer(Arc<Inner>);

impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let line = LogLine {
            time_unix: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs(),
            level: LogLevel::from_tracing(*event.metadata().level()),
            target: event.metadata().target().to_string(),
            message: visitor.finish(),
        };
        let mut lines = self.0.lines.lock().unwrap();
        if lines.len() >= MAX_LINES {
            lines.pop_front();
        }
        lines.push_back(line);
        drop(lines);
        self.0.version.fetch_add(1, Ordering::Relaxed);
    }
}

/// Renders a tracing event's fields into one line: the `message` field (every
/// `tracing::info!("text", ...)` call has one) first, then any other fields
/// as `name=value`, space-separated — so `tracing::warn!(id, error = %e,
/// "could not reserve a slot")` reads as `could not reserve a slot id=3
/// error=timed out`.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    extra: String,
}

impl MessageVisitor {
    fn finish(mut self) -> String {
        if !self.extra.is_empty() {
            if !self.message.is_empty() {
                self.message.push(' ');
            }
            self.message.push_str(&self.extra);
        }
        self.message
    }
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            if !self.extra.is_empty() {
                self.extra.push(' ');
            }
            self.extra.push_str(&format!("{}={:?}", field.name(), value));
        }
    }
}

static HANDLE: OnceLock<LogHandle> = OnceLock::new();

/// Install a process-global `tracing` subscriber that captures every log line
/// (from every crate, not just `app-state`) into a bounded in-memory ring
/// buffer, and returns a handle to read it. Also honours `RUST_LOG` (default
/// `info`), same as the `accelerator` daemon. Idempotent: call it once at
/// start-up; later calls (e.g. from tests) just return the same handle
/// without re-installing anything.
pub fn init() -> LogHandle {
    HANDLE
        .get_or_init(|| {
            let inner =
                Arc::new(Inner { lines: Mutex::new(VecDeque::new()), version: AtomicU64::new(0) });
            let handle = LogHandle(inner.clone());
            let filter =
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
            // `try_init` rather than `init`: a second call (e.g. a test that
            // also wants capture) must not panic the process.
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(CaptureLayer(inner))
                .try_init();
            handle
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test, not two: `init()`'s subscriber is process-global (a `OnceLock`),
    // so separate `#[test]`s racing on the same buffer from different threads
    // would be flaky against each other's `clear()`/log calls.
    #[test]
    fn captures_lines_and_the_ring_buffer_trims_to_max_lines() {
        let handle = init();
        handle.clear();

        tracing::warn!(id = 7, error = "boom", "could not reserve a slot");
        let lines = handle.snapshot();
        let last = lines.last().expect("a line was captured");
        assert_eq!(last.level, LogLevel::Warn);
        assert!(last.message.contains("could not reserve a slot"));
        assert!(last.message.contains("id=7"));
        assert!(last.message.contains("error=\"boom\"") || last.message.contains("error=boom"));

        handle.clear();
        for i in 0..(MAX_LINES + 10) {
            tracing::info!(i, "filler");
        }
        assert_eq!(handle.snapshot().len(), MAX_LINES);
    }
}
