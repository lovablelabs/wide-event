//! A `tracing_subscriber::Layer` that intercepts wide events, pulls the
//! finalized record from the thread-local emit stack, formats the
//! timestamp via [`FormatTime`], and serializes in a single pass.

use std::cell::RefCell;
use std::io::Write;
use std::sync::Mutex;

use tracing::Subscriber;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use crate::format::{JsonFormatter, WideEventFormatter};
use crate::{Rfc3339, DEFAULT_TARGET, EMIT_STACK};

thread_local! {
    static TIMESTAMP_BUF: RefCell<String> = const { RefCell::new(String::new()) };
}

/// A `tracing_subscriber::Layer` that writes wide event output.
///
/// Timestamp precision is controlled by the [`FormatTime`] implementation
/// (default: [`Rfc3339`](crate::Rfc3339)). Swap it with [`with_timer`](Self::with_timer):
///
/// ```no_run
/// use wide_event::WideEventLayer;
/// use tracing_subscriber::fmt::time::Uptime;
/// # use tracing_subscriber::prelude::*;
///
/// tracing_subscriber::registry()
///     .with(WideEventLayer::stdout().with_timer(Uptime::default()))
///     .init();
/// ```
pub struct WideEventLayer<W = std::io::Stdout, F = JsonFormatter, T = Rfc3339> {
    writer: Mutex<W>,
    formatter: F,
    timer: T,
    system: Option<&'static str>,
    target_prefix: &'static str,
}

impl WideEventLayer<std::io::Stdout, JsonFormatter, Rfc3339> {
    /// Create a layer that writes JSON wide events to stdout with
    /// RFC 3339 timestamps.
    #[must_use]
    pub fn stdout() -> Self {
        Self {
            writer: Mutex::new(std::io::stdout()),
            formatter: JsonFormatter,
            timer: Rfc3339,
            system: None,
            target_prefix: DEFAULT_TARGET,
        }
    }
}

impl<W: Write + Send + 'static, F: WideEventFormatter> WideEventLayer<W, F, Rfc3339> {
    /// Create a layer with a custom writer and formatter.
    ///
    /// Uses [`Rfc3339`](crate::Rfc3339) timestamps by default.
    /// Call [`with_timer`](Self::with_timer) to change.
    pub fn new(writer: W, formatter: F) -> Self {
        Self {
            writer: Mutex::new(writer),
            formatter,
            timer: Rfc3339,
            system: None,
            target_prefix: DEFAULT_TARGET,
        }
    }
}

impl<W: Write + Send + 'static, F: WideEventFormatter, T: FormatTime> WideEventLayer<W, F, T> {
    /// Set a process-wide `"system"` field injected into every wide event.
    #[must_use]
    pub fn with_system(mut self, system: &'static str) -> Self {
        self.system = Some(system);
        self
    }

    /// Set the target prefix for wide event output.
    ///
    /// The actual tracing target is always `wide_event` (a macro
    /// limitation), but the formatted output uses `{prefix}::{subsystem}`
    /// as the subsystem identifier.
    #[must_use]
    pub fn with_target_prefix(mut self, prefix: &'static str) -> Self {
        self.target_prefix = prefix;
        self
    }

    /// Set the timestamp formatter.
    ///
    /// Accepts any [`FormatTime`] implementation from `tracing_subscriber::fmt::time`.
    pub fn with_timer<T2: FormatTime>(self, timer: T2) -> WideEventLayer<W, F, T2> {
        WideEventLayer {
            writer: self.writer,
            formatter: self.formatter,
            timer,
            system: self.system,
            target_prefix: self.target_prefix,
        }
    }
}

impl<S, W, F, T> Layer<S> for WideEventLayer<W, F, T>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: Write + Send + 'static,
    F: WideEventFormatter + 'static,
    T: FormatTime + Send + Sync + 'static,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        // Match by the presence of wide_event_id field
        let mut visitor = IdVisitor(None);
        event.record(&mut visitor);
        let Some(id) = visitor.0 else { return };

        EMIT_STACK.with(|s| {
            let s = s.borrow();
            if let Some((_, record)) = s.iter().rev().find(|(rid, _)| *rid == id) {
                TIMESTAMP_BUF.with(|buf| {
                    let mut timestamp = buf.borrow_mut();
                    timestamp.clear();
                    let _ = self.timer.format_time(&mut Writer::new(&mut *timestamp));

                    if let Ok(mut w) = self.writer.lock() {
                        let _ =
                            self.formatter
                                .write_record(&mut *w, self.system, &timestamp, record);
                        let _ = w.write_all(b"\n");
                    }
                });
            }
        });
    }
}

/// Filter function that excludes wide events from other layers.
///
/// Uses the default target (`wide_event`).
///
/// ```no_run
/// use wide_event::{WideEventLayer, exclude_wide_events};
/// use tracing_subscriber::prelude::*;
/// use tracing_subscriber::filter::filter_fn;
///
/// tracing_subscriber::registry()
///     .with(WideEventLayer::stdout())
///     .with(
///         tracing_subscriber::fmt::layer()
///             .with_filter(filter_fn(exclude_wide_events))
///     )
///     .init();
/// ```
#[must_use]
pub fn exclude_wide_events(meta: &tracing::Metadata<'_>) -> bool {
    meta.target() != DEFAULT_TARGET
}

struct IdVisitor(Option<u64>);

impl tracing::field::Visit for IdVisitor {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "wide_event_id" {
            self.0 = Some(value);
        }
    }

    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WideEvent;
    use std::sync::Arc;
    use tracing_subscriber::prelude::*;

    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn layer_captures_json() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));

        let layer =
            WideEventLayer::new(SharedBuf(buf.clone()), JsonFormatter).with_system("testapp");
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let evt = WideEvent::new("ingress");
            evt.set_str("hello", "world");
            evt.emit();
        });

        let output = buf.lock().unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(std::str::from_utf8(&output).unwrap().trim()).unwrap();
        assert_eq!(parsed["hello"], "world");
        assert_eq!(parsed["system"], "testapp");
        assert_eq!(parsed["subsystem"], "ingress");
        // Timestamp is formatted by Rfc3339 timer
        assert!(parsed["timestamp"].as_str().unwrap().contains('T'));
    }

    #[test]
    fn layer_with_custom_timer() {
        use tracing_subscriber::fmt::time::Uptime;

        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));

        let layer = WideEventLayer::new(SharedBuf(buf.clone()), JsonFormatter)
            .with_timer(Uptime::default());
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let evt = WideEvent::new("http");
            evt.emit();
        });

        let output = buf.lock().unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(std::str::from_utf8(&output).unwrap().trim()).unwrap();
        // Uptime timer produces elapsed time, not RFC 3339
        let ts = parsed["timestamp"].as_str().unwrap();
        assert!(!ts.contains('T'));
    }

    #[test]
    fn layer_captures_logfmt() {
        use crate::LogfmtFormatter;

        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let layer = WideEventLayer::new(SharedBuf(buf.clone()), LogfmtFormatter);
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let evt = WideEvent::new("http");
            evt.set_str("method", "GET");
            evt.emit();
        });

        let logfmt = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert!(logfmt.contains("subsystem=http"));
        assert!(logfmt.contains("method=GET"));
    }

    #[test]
    fn non_wide_events_are_ignored() {
        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let layer = WideEventLayer::new(SharedBuf(buf.clone()), JsonFormatter);
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("normal log line");
        });

        assert!(buf.lock().unwrap().is_empty());
    }

    #[test]
    fn guard_emits_through_layer() {
        use crate::WideEventGuard;

        let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let layer = WideEventLayer::new(SharedBuf(buf.clone()), JsonFormatter);
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            let guard = WideEventGuard::new("http");
            guard.set_str("path", "/api");
            drop(guard);
        });

        let output = buf.lock().unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(std::str::from_utf8(&output).unwrap().trim()).unwrap();
        assert_eq!(parsed["path"], "/api");
        assert_eq!(parsed["subsystem"], "http");
    }

    #[test]
    fn emit_hook_sees_duration_ns() {
        let seen_duration = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen_clone = seen_duration.clone();

        let evt = WideEvent::new("test");
        evt.set_emit_hook(Arc::new(move |fields| {
            if fields.contains_key("duration_ns") {
                seen_clone.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }));
        evt.emit();

        assert!(seen_duration.load(std::sync::atomic::Ordering::SeqCst));
    }
}
