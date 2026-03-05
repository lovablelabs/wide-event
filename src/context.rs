//! Async task-local wide event propagation.
//!
//! Requires the `tokio` feature.
//!
//! # Example
//!
//! ```no_run
//! use wide_event::context;
//!
//! # async fn handle_request() {
//! context::scope("http", async {
//!     // anywhere in this async block:
//!     if let Some(evt) = context::current() {
//!         evt.set_str("user_id", "u-123");
//!     }
//!
//!     do_work().await;
//! }).await;
//! // ← wide event emitted automatically
//! # }
//! # async fn do_work() {}
//! ```

use std::sync::Arc;

use crate::WideEvent;

tokio::task_local! {
    static CURRENT: Arc<WideEvent>;
}

/// Get the current wide event from task-local storage.
///
/// Returns `None` if called outside of a [`scope`].
#[must_use]
pub fn current() -> Option<Arc<WideEvent>> {
    CURRENT.try_with(Clone::clone).ok()
}

/// Run a future with a wide event in task-local storage.
///
/// Creates a new [`WideEvent`] for the given subsystem, makes it
/// available via [`current()`], runs the future, and emits the event
/// when the future completes.
pub async fn scope<F: std::future::Future>(subsystem: &'static str, f: F) -> F::Output {
    scope_with(Arc::new(WideEvent::new(subsystem)), f).await
}

/// Run a future with an existing wide event in task-local storage.
///
/// The event is emitted automatically when the future completes.
/// If `emit()` was already called on the event, the auto-emit is a no-op.
pub async fn scope_with<F: std::future::Future>(
    event: Arc<WideEvent>,
    f: F,
) -> F::Output {
    let emit_ref = event.clone();
    let result = CURRENT.scope(event, f).await;
    emit_ref.emit();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn scope_emits_on_completion() {
        let emitted = Arc::new(AtomicBool::new(false));
        let emitted_clone = emitted.clone();

        scope("test", async {
            let evt = current().unwrap();
            evt.set_str("key", "value");
            evt.set_emit_hook(Arc::new(move |fields| {
                emitted_clone.store(true, Ordering::SeqCst);
                assert!(fields.contains_key("key"));
            }));
        })
        .await;

        assert!(emitted.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn current_returns_none_outside_scope() {
        assert!(current().is_none());
    }

    #[tokio::test]
    async fn manual_emit_prevents_double() {
        let count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let count_clone = count.clone();

        scope("test", async {
            let evt = current().unwrap();
            evt.set_emit_hook(Arc::new(move |_| {
                count_clone.fetch_add(1, Ordering::SeqCst);
            }));
            evt.emit(); // manual emit
        })
        .await; // auto-emit is no-op

        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
