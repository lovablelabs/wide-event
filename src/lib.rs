//! # wide-event
//!
//! Honeycomb-style wide events for Rust.
//!
//! A wide event accumulates key-value pairs throughout a request (or task)
//! lifecycle and emits them as a **single structured event** when the
//! request completes. This gives you one row per request in your log
//! aggregator with every dimension attached — perfect for high-cardinality
//! exploratory analysis.
//!
//! ## Quick start
//!
//! ```no_run
//! use wide_event::{WideEventGuard, WideEventLayer};
//! use tracing_subscriber::prelude::*;
//!
//! // Once at startup:
//! tracing_subscriber::registry()
//!     .with(WideEventLayer::stdout().with_system("myapp"))
//!     .init();
//!
//! // Per request — guard auto-emits on drop:
//! {
//!     let req = WideEventGuard::new("http");
//!     req.set_str("method", "GET");
//!     req.set_str("path", "/api/users");
//!     req.set_u64("status", 200);
//! } // ← emitted here as single JSON line
//! ```
//!
//! ## How it works
//!
//! 1. [`WideEvent::new`] starts a timer and creates an empty field map.
//! 2. Throughout processing, call setters (`set_str`, `set_u64`, `incr`,
//!    etc.) to accumulate fields — these are cheap local `Mutex` operations
//!    that never touch the tracing subscriber.
//! 3. [`WideEvent::emit`] (or [`WideEventGuard`] drop) finalizes the record,
//!    pushes it to a thread-local stack, and dispatches a structured
//!    `tracing::info!` event. The [`WideEventLayer`] pulls the record
//!    from the stack, formats the timestamp using the configured
//!    [`FormatTime`] implementation, and serializes in a single pass.
//!
//! ## Timestamp control
//!
//! Timestamps are formatted by a [`FormatTime`] implementation configured
//! on the layer. The default is [`Rfc3339`] (e.g. `2024-01-15T14:30:00.123Z`).
//! Swap it for any `tracing_subscriber::fmt::time` implementation:
//!
//! ```no_run
//! use wide_event::WideEventLayer;
//! use tracing_subscriber::fmt::time::Uptime;
//! # use tracing_subscriber::prelude::*;
//!
//! tracing_subscriber::registry()
//!     .with(WideEventLayer::stdout().with_timer(Uptime::default()))
//!     .init();
//! ```
//!
//! ## Features
//!
//! - **`opentelemetry`** — attaches `trace_id` and `span_id` from the
//!   current OpenTelemetry span context.
//! - **`tokio`** — provides [`context::scope`] and [`context::current`]
//!   for async task-local wide event propagation.

#![warn(clippy::pedantic, missing_docs)]

mod format;
mod layer;

#[cfg(feature = "tokio")]
pub mod context;

pub use format::{JsonFormatter, LogfmtFormatter, WideEventFormatter};
pub use layer::{exclude_wide_events, WideEventLayer};
pub use tracing_subscriber::fmt::time::FormatTime;

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::Value;
use tracing_subscriber::fmt::format::Writer;

/// Default target used for wide event tracing dispatches.
pub const DEFAULT_TARGET: &str = "wide_event";

/// Callback invoked just before serialization with the accumulated fields.
pub type EmitHook = Arc<dyn Fn(&HashMap<&'static str, Value>) + Send + Sync>;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    pub(crate) static EMIT_STACK: RefCell<Vec<(u64, WideEventRecord)>> =
        const { RefCell::new(Vec::new()) };
}

/// Default timer: RFC 3339 timestamps with microsecond precision.
///
/// Produces timestamps like `2024-01-15T14:30:00.123456Z`.
pub struct Rfc3339;

impl FormatTime for Rfc3339 {
    #[allow(
        clippy::many_single_char_names,
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::cast_sign_loss,
        reason = "variable names match the Howard Hinnant civil date algorithm; casts are safe by algorithm invariants"
    )]
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        let now = std::time::SystemTime::now();
        let dur = now
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = dur.as_secs();
        let micros = dur.subsec_micros();

        // Convert epoch seconds to date/time components
        let days = (secs / 86400) as i64;
        let time_of_day = secs % 86400;
        let hours = time_of_day / 3600;
        let minutes = (time_of_day % 3600) / 60;
        let seconds = time_of_day % 60;

        // Civil date from days since epoch (algorithm from Howard Hinnant)
        let z = days + 719_468;
        let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
        let doe = (z - era * 146_097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };

        write!(
            w,
            "{y:04}-{m:02}-{d:02}T{hours:02}:{minutes:02}:{seconds:02}.{micros:06}Z",
        )
    }
}

/// A finalized wide event record ready for formatting.
///
/// Contains the accumulated fields and timing data. The timestamp
/// is formatted separately by the layer's [`FormatTime`] implementation.
pub struct WideEventRecord {
    /// The subsystem label (e.g. `"http"`, `"grpc"`).
    pub subsystem: &'static str,
    /// Wall-clock duration from event creation to emit.
    pub duration: std::time::Duration,
    /// Accumulated key-value fields.
    pub fields: HashMap<&'static str, Value>,
    /// OpenTelemetry trace ID, if the `opentelemetry` feature is enabled.
    pub trace_id: Option<String>,
    /// OpenTelemetry span ID, if the `opentelemetry` feature is enabled.
    pub span_id: Option<String>,
}

/// A wide event that accumulates structured fields over its lifetime.
///
/// Field setters are cheap — they only touch a local `Mutex<HashMap>`,
/// never the tracing subscriber stack.
pub struct WideEvent {
    subsystem: &'static str,
    inner: Mutex<WideEventInner>,
}

struct WideEventInner {
    fields: HashMap<&'static str, Value>,
    start: Instant,
    emit_hook: Option<EmitHook>,
    emitted: bool,
}

impl std::fmt::Debug for WideEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WideEvent")
            .field("subsystem", &self.subsystem)
            .finish_non_exhaustive()
    }
}

#[allow(
    clippy::missing_panics_doc,
    reason = "all panics are from Mutex::lock which only panics if poisoned"
)]
impl WideEvent {
    /// Create a new wide event for the given subsystem.
    ///
    /// To set a process-wide `"system"` field, configure it on the
    /// [`WideEventLayer`] via [`WideEventLayer::with_system`].
    #[must_use]
    pub fn new(subsystem: &'static str) -> Self {
        Self {
            subsystem,
            inner: Mutex::new(WideEventInner {
                fields: HashMap::with_capacity(24),
                start: Instant::now(),
                emit_hook: None,
                emitted: false,
            }),
        }
    }

    /// Set a hook called with the accumulated fields just before emit.
    pub fn set_emit_hook(&self, hook: EmitHook) {
        self.inner.lock().unwrap().emit_hook = Some(hook);
    }

    /// Returns true if the given key exists in the accumulated fields.
    pub fn has_key(&self, key: &str) -> bool {
        self.inner.lock().unwrap().fields.contains_key(key)
    }

    /// Set a string field (copies `val`; use [`set_string`](Self::set_string) to move an owned `String`).
    pub fn set_str(&self, key: &'static str, val: &str) {
        self.inner
            .lock()
            .unwrap()
            .fields
            .insert(key, Value::String(val.to_string()));
    }

    /// Set a string field from an owned `String` (avoids a copy).
    pub fn set_string(&self, key: &'static str, val: String) {
        self.inner
            .lock()
            .unwrap()
            .fields
            .insert(key, Value::String(val));
    }

    /// Set a signed 64-bit integer field.
    pub fn set_i64(&self, key: &'static str, val: i64) {
        self.inner
            .lock()
            .unwrap()
            .fields
            .insert(key, Value::Number(val.into()));
    }

    /// Set an unsigned 64-bit integer field.
    pub fn set_u64(&self, key: &'static str, val: u64) {
        self.inner
            .lock()
            .unwrap()
            .fields
            .insert(key, Value::Number(val.into()));
    }

    /// Set a 64-bit floating-point field (`NaN` / `Inf` are stored as `null`).
    pub fn set_f64(&self, key: &'static str, val: f64) {
        self.inner.lock().unwrap().fields.insert(
            key,
            serde_json::Number::from_f64(val).map_or(Value::Null, Value::Number),
        );
    }

    /// Set a boolean field.
    pub fn set_bool(&self, key: &'static str, val: bool) {
        self.inner
            .lock()
            .unwrap()
            .fields
            .insert(key, Value::Bool(val));
    }

    /// Set a field from a raw [`serde_json::Value`].
    pub fn set_value(&self, key: &'static str, val: Value) {
        self.inner.lock().unwrap().fields.insert(key, val);
    }

    /// Increment an integer counter field by 1 (initialized to 1 if absent).
    pub fn incr(&self, key: &'static str) {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.fields.entry(key).or_insert(Value::Number(0.into()));
        if let Some(n) = entry.as_i64() {
            *entry = Value::Number((n + 1).into());
        }
    }

    /// Set `error = true`, `error.type`, and `error.message`.
    pub fn set_error(&self, err_type: &str, message: &str) {
        self.set_bool("error", true);
        self.set_str("error.type", err_type);
        self.set_str("error.message", message);
    }

    /// Emit the wide event through the tracing pipeline.
    ///
    /// The tracing target is `{target_prefix}::{subsystem}` where
    /// `target_prefix` defaults to `wide_event`. Configure the prefix
    /// on [`WideEventLayer::with_target_prefix`].
    ///
    /// Calling `emit()` more than once is a no-op.
    pub fn emit(&self) {
        let Some(record) = self.finalize() else {
            return;
        };
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let subsystem = self.subsystem;

        EMIT_STACK.with(|s| s.borrow_mut().push((id, record)));

        // Target must be a string literal for tracing macros. We use the
        // default target here; the layer matches by the wide_event_id field.
        tracing::info!(
            target: "wide_event",
            wide_event_id = id,
            subsystem = subsystem,
        );

        EMIT_STACK.with(|s| {
            s.borrow_mut().pop();
        });
    }

    #[allow(
        clippy::items_after_statements,
        reason = "cfg-gated inner fn for conditional compilation is idiomatic"
    )]
    fn finalize(&self) -> Option<WideEventRecord> {
        let mut inner = self.inner.lock().unwrap();
        if inner.emitted {
            return None;
        }
        inner.emitted = true;

        let duration = inner.start.elapsed();

        // Insert duration into fields so emit hooks can access it
        #[allow(
            clippy::cast_possible_truncation,
            reason = "duration_ns fits in u64 for any practical duration"
        )]
        inner.fields.insert(
            "duration_ns",
            Value::Number((duration.as_nanos() as u64).into()),
        );

        let mut fields = std::mem::take(&mut inner.fields);

        if let Some(ref hook) = inner.emit_hook {
            hook(&fields);
        }

        // Remove duration_ns from fields to avoid double-emission
        // (the formatter emits it from `record.duration`)
        fields.remove("duration_ns");

        let (trace_id, span_id) = otel_context();

        #[cfg(feature = "opentelemetry")]
        fn otel_context() -> (Option<String>, Option<String>) {
            use opentelemetry::trace::TraceContextExt;
            use tracing_opentelemetry::OpenTelemetrySpanExt;

            let span = tracing::Span::current();
            let cx = span.context();
            let sc = cx.span().span_context().clone();
            if sc.is_valid() {
                (
                    Some(format!("{:032x}", sc.trace_id())), // TraceId has no Display impl with padding
                    Some(format!("{:016x}", sc.span_id())), // SpanId has no Display impl with padding
                )
            } else {
                (None, None)
            }
        }

        #[cfg(not(feature = "opentelemetry"))]
        fn otel_context() -> (Option<String>, Option<String>) {
            (None, None)
        }

        Some(WideEventRecord {
            subsystem: self.subsystem,
            duration,
            fields,
            trace_id,
            span_id,
        })
    }
}

/// RAII guard that emits the wide event when dropped.
///
/// Implements `Deref<Target = WideEvent>` so you can call setters directly.
///
/// ```
/// # use wide_event::WideEventGuard;
/// let req = WideEventGuard::new("http");
/// req.set_str("method", "GET");
/// req.set_u64("status", 200);
/// // emitted automatically when `req` drops
/// ```
pub struct WideEventGuard(WideEvent);

impl WideEventGuard {
    /// Create a new guard that emits on drop.
    #[must_use]
    pub fn new(subsystem: &'static str) -> Self {
        Self(WideEvent::new(subsystem))
    }
}

impl Drop for WideEventGuard {
    fn drop(&mut self) {
        self.0.emit();
    }
}

impl std::ops::Deref for WideEventGuard {
    type Target = WideEvent;
    fn deref(&self) -> &WideEvent {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn set_and_has_key() {
        let evt = WideEvent::new("test");
        assert!(!evt.has_key("foo"));
        evt.set_str("foo", "bar");
        assert!(evt.has_key("foo"));
    }

    #[test]
    fn incr_creates_and_increments() {
        let evt = WideEvent::new("test");
        evt.incr("counter");
        evt.incr("counter");
        evt.incr("counter");
        let inner = evt.inner.lock().unwrap();
        assert_eq!(inner.fields["counter"], Value::Number(3.into()));
    }

    #[test]
    fn set_error_sets_three_fields() {
        let evt = WideEvent::new("test");
        evt.set_error("timeout", "connection timed out");
        let inner = evt.inner.lock().unwrap();
        assert_eq!(inner.fields["error"], Value::Bool(true));
        assert_eq!(
            inner.fields["error.type"],
            Value::String("timeout".to_string())
        );
    }

    #[test]
    fn double_emit_is_noop() {
        let count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let count_clone = count.clone();

        let evt = WideEvent::new("test");
        evt.set_emit_hook(Arc::new(move |_| {
            count_clone.fetch_add(1, Ordering::SeqCst);
        }));
        evt.emit();
        evt.emit();
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn finalize_moves_fields() {
        let evt = WideEvent::new("test");
        evt.set_str("key", "value");

        let record = evt.finalize().unwrap();
        assert_eq!(record.fields["key"], Value::String("value".to_string()));
        assert_eq!(record.subsystem, "test");
        assert!(evt.finalize().is_none());
    }

    #[test]
    fn guard_auto_emits() {
        let emitted = Arc::new(AtomicBool::new(false));
        let emitted_clone = emitted.clone();

        {
            let guard = WideEventGuard::new("test");
            guard.set_emit_hook(Arc::new(move |_| {
                emitted_clone.store(true, Ordering::SeqCst);
            }));
            guard.set_str("method", "GET");
        }

        assert!(emitted.load(Ordering::SeqCst));
    }

    #[test]
    fn guard_deref() {
        let guard = WideEventGuard::new("http");
        guard.set_str("path", "/api");
        assert!(guard.has_key("path"));
        guard.emit(); // manual emit; Drop will be no-op
    }

    #[test]
    fn rfc3339_timer() {
        let mut buf = String::new();
        Rfc3339.format_time(&mut Writer::new(&mut buf)).unwrap();
        assert!(buf.contains('T'));
        assert!(buf.ends_with('Z'));
    }
}
