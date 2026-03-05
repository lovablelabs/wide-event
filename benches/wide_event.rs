#![allow(clippy::cast_sign_loss, clippy::cast_lossless, reason = "benchmark code with trivially safe casts")]

use std::io::Write;
use std::sync::{Arc, Mutex};

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use tracing::dispatcher::Dispatch;
use tracing_subscriber::prelude::*;
use wide_event::{JsonFormatter, LogfmtFormatter, WideEvent, WideEventLayer};

/// A cloneable sink backed by a shared `Vec<u8>`.
#[derive(Clone)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl SharedBuf {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::with_capacity(4096))))
    }

    fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn bench_set_str(c: &mut Criterion) {
    c.bench_function("set_str", |b| {
        let evt = WideEvent::new("bench");
        b.iter(|| {
            evt.set_str(black_box("key"), black_box("value"));
        });
    });
}

fn bench_set_u64(c: &mut Criterion) {
    c.bench_function("set_u64", |b| {
        let evt = WideEvent::new("bench");
        b.iter(|| {
            evt.set_u64(black_box("counter"), black_box(42));
        });
    });
}

fn bench_set_f64(c: &mut Criterion) {
    c.bench_function("set_f64", |b| {
        let evt = WideEvent::new("bench");
        b.iter(|| {
            evt.set_f64(black_box("latency"), black_box(3.15));
        });
    });
}

fn bench_incr(c: &mut Criterion) {
    c.bench_function("incr", |b| {
        let evt = WideEvent::new("bench");
        b.iter(|| {
            evt.incr(black_box("counter"));
        });
    });
}

fn bench_accumulate_20_fields(c: &mut Criterion) {
    c.bench_function("accumulate_20_fields", |b| {
        b.iter(|| {
            let evt = WideEvent::new("bench");
            for i in 0..7 {
                evt.set_str(black_box(leak_str(format!("str_{i}"))), black_box("val"));
            }
            for i in 0..7 {
                evt.set_u64(black_box(leak_str(format!("u64_{i}"))), black_box(i as u64));
            }
            for i in 0..6 {
                evt.set_f64(black_box(leak_str(format!("f64_{i}"))), black_box(i as f64));
            }
        });
    });
}

fn bench_emit_10_fields(c: &mut Criterion) {
    let buf = SharedBuf::new();
    let layer = WideEventLayer::new(buf.clone(), JsonFormatter);
    let dispatch = Dispatch::new(tracing_subscriber::registry().with(layer));

    c.bench_function("emit_10_fields", |b| {
        b.iter(|| {
            tracing::dispatcher::with_default(&dispatch, || {
                let evt = WideEvent::new("bench");
                for i in 0..5 {
                    evt.set_str(black_box(leak_str(format!("str_{i}"))), black_box("val"));
                }
                for i in 0..5 {
                    evt.set_u64(black_box(leak_str(format!("u64_{i}"))), black_box(i as u64));
                }
                evt.emit();
            });
            buf.clear();
        });
    });
}

fn bench_emit_json_20_fields(c: &mut Criterion) {
    let buf = SharedBuf::new();
    let layer = WideEventLayer::new(buf.clone(), JsonFormatter);
    let dispatch = Dispatch::new(tracing_subscriber::registry().with(layer));

    c.bench_function("emit_json_20_fields", |b| {
        b.iter(|| {
            tracing::dispatcher::with_default(&dispatch, || {
                let evt = WideEvent::new("bench");
                for i in 0..7 {
                    evt.set_str(black_box(leak_str(format!("str_{i}"))), black_box("val"));
                }
                for i in 0..7 {
                    evt.set_u64(black_box(leak_str(format!("u64_{i}"))), black_box(i as u64));
                }
                for i in 0..6 {
                    evt.set_f64(black_box(leak_str(format!("f64_{i}"))), black_box(i as f64));
                }
                evt.emit();
            });
            buf.clear();
        });
    });
}

fn bench_emit_logfmt_20_fields(c: &mut Criterion) {
    let buf = SharedBuf::new();
    let layer = WideEventLayer::new(buf.clone(), LogfmtFormatter);
    let dispatch = Dispatch::new(tracing_subscriber::registry().with(layer));

    c.bench_function("emit_logfmt_20_fields", |b| {
        b.iter(|| {
            tracing::dispatcher::with_default(&dispatch, || {
                let evt = WideEvent::new("bench");
                for i in 0..7 {
                    evt.set_str(black_box(leak_str(format!("str_{i}"))), black_box("val"));
                }
                for i in 0..7 {
                    evt.set_u64(black_box(leak_str(format!("u64_{i}"))), black_box(i as u64));
                }
                for i in 0..6 {
                    evt.set_f64(black_box(leak_str(format!("f64_{i}"))), black_box(i as f64));
                }
                evt.emit();
            });
            buf.clear();
        });
    });
}

/// Leak a string to get a `&'static str` for benchmark field keys.
fn leak_str(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

criterion_group!(
    benches,
    bench_set_str,
    bench_set_u64,
    bench_set_f64,
    bench_incr,
    bench_accumulate_20_fields,
    bench_emit_10_fields,
    bench_emit_json_20_fields,
    bench_emit_logfmt_20_fields,
);
criterion_main!(benches);
