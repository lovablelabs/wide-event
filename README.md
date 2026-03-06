# wide-event

[![Crates.io](https://img.shields.io/crates/v/wide-event.svg)](https://crates.io/crates/wide-event)
[![docs.rs](https://docs.rs/wide-event/badge.svg)](https://docs.rs/wide-event)
[![License](https://img.shields.io/crates/l/wide-event.svg)](LICENSE-MIT)

[Wide events](https://charity.wtf/2022/08/15/live-your-best-life-with-structured-events/)
for Rust, built on `tracing`. One structured event per request with all
fields accumulated over the request lifecycle, emitted as a single JSON
line (or logfmt) on completion.

## Why

A span tree for one request can be 10–30 spans. A wide event is one row
with all the dimensions attached. Cheaper to store and query than a span
tree, and retains per-request detail that pre-aggregated metrics lose.

### Metrics from events

An emit hook receives the accumulated fields before serialization. You
can update Prometheus counters and histograms from the event data
directly, which keeps metric definitions next to the event they describe
rather than scattered as `counter.inc()` calls across the codebase.

This means you don't end up with a counter that's incremented on a code
path that doesn't emit enough context to debug it, or an event that's
missing the metric update. The event is the source of truth; metrics are
a view of it.

### Dynamic fields

With tracing spans, fields must be declared at span creation as `Empty`
slots, and each `Span::record` call dispatches through the full
subscriber stack. Wide event setters are `HashMap` inserts behind a local
`Mutex` — any key, any time, no subscriber dispatch until emit.

This matters when telemetry is contributed by several layers that don't
know about each other's fields. Middleware sets `auth.user_id`, the
handler sets `project_id`, the proxy layer sets `upstream_latency_ns`,
error recovery sets `error.type`. Each just calls `event.set_str(...)` on
the shared `WideEvent` without needing a reference to a root span or
knowing what other fields exist.

### Complements tracing

Wide events and tracing spans answer different questions. Spans give you
the call tree — where time was spent within a request. Wide events give
you the outcome — one flat row you can filter and aggregate across
requests.

With the `opentelemetry` feature enabled, `trace_id` and `span_id` from
the current span context are attached to the event automatically. So you
can query wide events to find interesting requests, then jump to the
trace for the ones that need deeper investigation.

## Usage

```rust
use wide_event::{WideEventGuard, WideEventLayer};
use tracing_subscriber::prelude::*;

tracing_subscriber::registry()
    .with(WideEventLayer::stdout().with_system("myapp"))
    .init();

{
    let req = WideEventGuard::new("http");
    req.set_str("method", "GET");
    req.set_str("path", "/api/users");
    req.set_u64("status", 200);
} // emitted here
```

`WideEventGuard` emits on drop. Use `WideEvent` directly if you want
explicit `emit()` control.

For async task propagation, enable the `tokio` feature and use
`context::scope` / `context::current`.

## Internals

1. `WideEvent::new` — starts a timer, allocates an empty field map.
2. Setters (`set_str`, `set_u64`, `set_bool`, `incr`, …) — `Mutex`
   lock + `HashMap::insert`. No subscriber dispatch.
3. `emit()` — finalizes the record, calls the emit hook if set, pushes
   to a thread-local stack, fires `tracing::info!`. The `WideEventLayer`
   pops the record and serializes it in a single pass.

Field keys are `&'static str` so setter calls don't allocate for the key.
Field names are almost always string literals, so this is a natural fit.

The thread-local emit stack avoids cross-thread synchronization between
`emit()` and the layer. The layer pulls the record from the stack during
the `tracing::info!` dispatch on the same thread.

## Features

| Feature | What it does |
|---------|-------------|
| `opentelemetry` | Attaches `trace_id` / `span_id` from current OTel context |
| `tokio` | Task-local propagation via `context::scope` / `context::current` |

```toml
[dependencies]
wide-event = { version = "0.1", features = ["tokio"] }
```

## Formatters

`JsonFormatter` (default) or `LogfmtFormatter`:

```rust
use wide_event::{WideEventLayer, LogfmtFormatter};
let layer = WideEventLayer::new(std::io::stdout(), LogfmtFormatter);
```

Custom timestamp formatting via `with_timer()` — accepts any
`tracing_subscriber::fmt::time::FormatTime` implementation:

```rust
use wide_event::WideEventLayer;
use tracing_subscriber::fmt::time::Uptime;

let layer = WideEventLayer::stdout().with_timer(Uptime::default());
```

The default timer is `Rfc3339` — microsecond-precision RFC 3339
timestamps using `humantime`.

## Development

```bash
git config core.hooksPath .githooks    # pre-push: fmt + clippy + test
cargo release patch                    # or: minor, major
```

Requires [`cargo-release`](https://crates.io/crates/cargo-release).

## License

MIT or Apache-2.0, at your option.
