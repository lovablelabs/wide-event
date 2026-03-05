//! Output formatters for wide events.
//!
//! - [`JsonFormatter`] — one JSON object per line (default)
//! - [`LogfmtFormatter`] — `key=value` pairs per line

use std::collections::HashMap;
use std::io::{self, Write};

use serde::Serialize;
use serde_json::Value;

use crate::WideEventRecord;

/// Trait for formatting a wide event record into a writer.
///
/// The `timestamp` parameter is pre-formatted by the layer's
/// [`FormatTime`](crate::FormatTime) implementation — formatters
/// should use it as-is.
pub trait WideEventFormatter: Send + Sync {
    /// Format a single wide event record and write it to `w`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if writing to `w` fails.
    fn write_record(
        &self,
        w: &mut dyn Write,
        system: Option<&str>,
        timestamp: &str,
        record: &WideEventRecord,
    ) -> io::Result<()>;
}

/// Formats wide events as single-line JSON objects.
pub struct JsonFormatter;

impl WideEventFormatter for JsonFormatter {
    fn write_record(
        &self,
        w: &mut dyn Write,
        system: Option<&str>,
        timestamp: &str,
        record: &WideEventRecord,
    ) -> io::Result<()> {
        #[derive(Serialize)]
        struct Output<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            system: Option<&'a str>,
            subsystem: &'a str,
            timestamp: &'a str,
            duration_ns: f64,
            #[serde(skip_serializing_if = "Option::is_none")]
            trace_id: &'a Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            span_id: &'a Option<String>,
            #[serde(flatten)]
            fields: &'a HashMap<&'static str, Value>,
        }

        let output = Output {
            system,
            subsystem: record.subsystem,
            timestamp,
            #[allow(clippy::cast_precision_loss, reason = "acceptable for duration display")]
            duration_ns: record.duration.as_nanos() as f64,
            trace_id: &record.trace_id,
            span_id: &record.span_id,
            fields: &record.fields,
        };

        serde_json::to_writer(w, &output).map_err(io::Error::other)
    }
}

/// Formats wide events as logfmt (`key=value` pairs).
pub struct LogfmtFormatter;

impl WideEventFormatter for LogfmtFormatter {
    fn write_record(
        &self,
        w: &mut dyn Write,
        system: Option<&str>,
        timestamp: &str,
        record: &WideEventRecord,
    ) -> io::Result<()> {
        let mut first = true;
        let mut pair = |w: &mut dyn Write, key: &str, val: &str| -> io::Result<()> {
            if !first {
                write!(w, " ")?;
            }
            first = false;
            if val.contains(' ') || val.contains('"') || val.contains('=') || val.is_empty() {
                write!(
                    w,
                    "{key}=\"{}\"",
                    val.replace('\\', "\\\\").replace('"', "\\\"")
                )
            } else {
                write!(w, "{key}={val}")
            }
        };

        if let Some(system) = system {
            pair(w, "system", system)?;
        }
        pair(w, "subsystem", record.subsystem)?;
        pair(w, "timestamp", timestamp)?;
        #[allow(clippy::cast_precision_loss, reason = "acceptable for duration display")]
        let duration_ns = record.duration.as_nanos() as f64;
        pair(w, "duration_ns", &duration_ns.to_string())?;

        if let Some(ref tid) = record.trace_id {
            pair(w, "trace_id", tid)?;
        }
        if let Some(ref sid) = record.span_id {
            pair(w, "span_id", sid)?;
        }

        for (k, v) in &record.fields {
            let val_str = match v {
                Value::String(s) => s.as_str().into(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => "null".into(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            pair(w, k, &val_str)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_record() -> WideEventRecord {
        let mut fields = HashMap::new();
        fields.insert("method", Value::String("GET".into()));
        fields.insert("status", Value::Number(200.into()));

        WideEventRecord {
            subsystem: "http",
            duration: std::time::Duration::from_nanos(1_500_000),
            fields,
            trace_id: None,
            span_id: None,
        }
    }

    #[test]
    fn json_basic() {
        let mut buf = Vec::new();
        JsonFormatter
            .write_record(&mut buf, Some("myapp"), "2024-01-15T14:30:00Z", &test_record())
            .unwrap();

        let parsed: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(parsed["system"], "myapp");
        assert_eq!(parsed["subsystem"], "http");
        assert_eq!(parsed["timestamp"], "2024-01-15T14:30:00Z");
        assert_eq!(parsed["method"], "GET");
        assert_eq!(parsed["status"], 200);
        assert_eq!(parsed["duration_ns"], 1_500_000.0);
    }

    #[test]
    fn json_no_system() {
        let mut buf = Vec::new();
        JsonFormatter
            .write_record(&mut buf, None, "2024-01-15T14:30:00Z", &test_record())
            .unwrap();

        let parsed: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert!(parsed.get("system").is_none());
    }

    #[test]
    fn json_with_otel() {
        let mut rec = test_record();
        rec.trace_id = Some("abc123".into());
        rec.span_id = Some("def456".into());

        let mut buf = Vec::new();
        JsonFormatter
            .write_record(&mut buf, None, "2024-01-15T14:30:00Z", &rec)
            .unwrap();

        let parsed: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(parsed["trace_id"], "abc123");
        assert_eq!(parsed["span_id"], "def456");
    }

    #[test]
    fn logfmt_basic() {
        let mut buf = Vec::new();
        LogfmtFormatter
            .write_record(&mut buf, Some("myapp"), "2024-01-15T14:30:00Z", &test_record())
            .unwrap();

        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("system=myapp"));
        assert!(output.contains("subsystem=http"));
        assert!(output.contains("timestamp=2024-01-15T14:30:00Z"));
        assert!(output.contains("duration_ns=1500000"));
    }

    #[test]
    fn logfmt_quotes_spaces() {
        let mut rec = test_record();
        rec.fields
            .insert("msg", Value::String("hello world".into()));

        let mut buf = Vec::new();
        LogfmtFormatter
            .write_record(&mut buf, None, "now", &rec)
            .unwrap();

        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("msg=\"hello world\""));
    }
}
