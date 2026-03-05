# wide-event

[![Crates.io](https://img.shields.io/crates/v/wide-event.svg)](https://crates.io/crates/wide-event)
[![docs.rs](https://docs.rs/wide-event/badge.svg)](https://docs.rs/wide-event)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE-MIT)

Honeycomb-style wide events for Rust.

A wide event accumulates key-value pairs throughout a request (or task)
lifecycle and emits them as a **single structured event** when the request
completes. This gives you one row per request in your log aggregator with
every dimension attached — perfect for high-cardinality exploratory analysis.

## Quick start

```rust
use wide_event::{WideEventGuard, WideEventLayer};
use tracing_subscriber::prelude::*;

// Once at startup:
tracing_subscriber::registry()
    .with(WideEventLayer::stdout().with_system("myapp"))
    .init();

// Per request — guard auto-emits on drop:
{
    let req = WideEventGuard::new("http");
    req.set_str("method", "GET");
    req.set_str("path", "/api/users");
    req.set_u64("status", 200);
} // ← emitted here as single JSON line
```

## How it works

1. `WideEvent::new` starts a timer and creates an empty field map.
2. Throughout processing, call setters (`set_str`, `set_u64`, `incr`, etc.)
   to accumulate fields — these are cheap local `Mutex` operations that never
   touch the tracing subscriber.
3. `WideEvent::emit` (or `WideEventGuard` drop) finalizes the record, pushes
   it to a thread-local stack, and dispatches a structured `tracing::info!`
   event. The `WideEventLayer` pulls the record from the stack, formats the
   timestamp, and serializes in a single pass.

## Features

| Feature | Description |
|---------|-------------|
| `opentelemetry` | Attaches `trace_id` and `span_id` from the current OpenTelemetry span context |
| `tokio` | Provides `context::scope` and `context::current` for async task-local wide event propagation |

```toml
[dependencies]
wide-event = { version = "0.1", features = ["tokio"] }
```

## Formatter options

- **`JsonFormatter`** (default) — one JSON object per line
- **`LogfmtFormatter`** — `key=value` pairs per line

```rust
use wide_event::{WideEventLayer, LogfmtFormatter};

let layer = WideEventLayer::new(std::io::stdout(), LogfmtFormatter);
```

## Performance

- Field keys are `&'static str` — zero allocation on every setter call. Field
  names are almost always string literals, so this is natural and avoids the
  `key.to_string()` overhead entirely.
- Field setters (`set_str`, `set_u64`, `incr`, …) are cheap local `Mutex`
  operations — they never interact with the tracing subscriber.
- Timestamp formatting reuses a thread-local buffer — no `String` allocation
  per emit.
- Serialization happens once at emit time in a single pass over the accumulated
  fields.
- A thread-local emit stack avoids cross-thread synchronization on the hot path.

## Development

```bash
# Set up pre-push hook (runs fmt, clippy, tests before each push)
git config core.hooksPath .githooks

# Release a new version (bumps Cargo.toml, commits, tags, pushes)
# CI publishes to crates.io automatically on tag push.
cargo release patch  # or: minor, major
```

Requires [`cargo-release`](https://crates.io/crates/cargo-release):
`cargo install cargo-release`

## License

MIT — see [LICENSE-MIT](LICENSE-MIT).
