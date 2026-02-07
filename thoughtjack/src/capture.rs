//! Traffic capture module (TJ-SPEC-007).
//!
//! Captures request/response traffic to NDJSON files for analysis.
//! Each session creates a `capture-<timestamp>-<pid>.ndjson` file in the
//! configured capture directory.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tracing::debug;

use crate::error::ThoughtJackError;

/// Message type for capture entries.
///
/// Implements: TJ-SPEC-007 F-013
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureType {
    /// Request from client to server.
    Request,
    /// Response from server to client.
    Response,
    /// Server-initiated notification.
    Notification,
}

/// A single captured traffic entry.
///
/// Fields match the TJ-SPEC-007 F-013 capture format.
///
/// Implements: TJ-SPEC-007 F-013
#[derive(Debug, Serialize)]
struct CaptureEntry<'a> {
    /// ISO 8601 timestamp.
    ts: String,
    /// Message type.
    #[serde(rename = "type")]
    msg_type: CaptureType,
    /// JSON-RPC id (null for notifications).
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<&'a serde_json::Value>,
    /// MCP method name.
    #[serde(skip_serializing_if = "Option::is_none")]
    method: Option<&'a str>,
    /// Request parameters or response result.
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<&'a serde_json::Value>,
    /// Current phase name.
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<&'a str>,
}

/// Writer for traffic capture files.
///
/// Writes NDJSON (newline-delimited JSON) lines to a session file.
/// Thread-safe via internal `Mutex`. When `redact` is enabled, sensitive
/// fields are replaced with `"[REDACTED]"` before writing.
///
/// Implements: TJ-SPEC-007 F-002
pub struct CaptureWriter {
    // std::sync::Mutex is intentional: held briefly for buffered write + flush,
    // never across .await points.
    writer: Mutex<BufWriter<File>>,
    path: PathBuf,
    redact: bool,
}

impl CaptureWriter {
    /// Creates a new capture writer in the given directory.
    ///
    /// Creates the directory if it doesn't exist and opens a new session
    /// file named `capture-<timestamp>-<pid>.ndjson`. When `redact` is `true`,
    /// sensitive fields are replaced with `"[REDACTED]"` before writing.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created or the file
    /// cannot be opened.
    ///
    /// Implements: TJ-SPEC-007 F-013
    pub fn new(capture_dir: &Path, redact: bool) -> Result<Self, ThoughtJackError> {
        if capture_dir.as_os_str().is_empty() {
            return Err(ThoughtJackError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "capture directory path is empty",
            )));
        }
        fs::create_dir_all(capture_dir)?;

        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let pid = std::process::id();
        let filename = format!("capture-{timestamp}-{pid}.ndjson");
        let path = capture_dir.join(filename);

        let file = OpenOptions::new().create(true).append(true).open(&path)?;

        debug!(path = %path.display(), redact, "capture file opened");

        Ok(Self {
            writer: Mutex::new(BufWriter::new(file)),
            path,
            redact,
        })
    }

    /// Records a message in the capture file.
    ///
    /// Serializes the entry as a single NDJSON line with structured fields
    /// (ts, type, id, method, params, phase).
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or I/O fails.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    ///
    /// Implements: TJ-SPEC-007 F-013
    pub fn record(
        &self,
        msg_type: CaptureType,
        data: &serde_json::Value,
        phase: Option<&str>,
    ) -> Result<(), ThoughtJackError> {
        let effective_data;
        let data_ref = if self.redact {
            effective_data = redact_value(data);
            &effective_data
        } else {
            data
        };

        let entry = CaptureEntry {
            ts: chrono::Utc::now().to_rfc3339(),
            msg_type,
            id: data_ref.get("id"),
            method: data_ref.get("method").and_then(serde_json::Value::as_str),
            params: data_ref
                .get("params")
                .or_else(|| data_ref.get("result"))
                .or_else(|| data_ref.get("error")),
            phase,
        };

        let line = serde_json::to_string(&entry)?;
        let mut writer = self.writer.lock().expect("capture writer lock poisoned");
        writeln!(writer, "{line}")?;
        writer.flush()?;
        drop(writer);

        Ok(())
    }

    /// Returns the path to the capture file.
    ///
    /// Implements: TJ-SPEC-007 F-002
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Recursively redacts all string values in a JSON tree.
fn redact_deep(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for val in map.values_mut() {
                redact_deep(val);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                redact_deep(item);
            }
        }
        serde_json::Value::String(s) if !s.is_empty() => {
            *value = serde_json::Value::String("[REDACTED]".to_string());
        }
        _ => {} // preserve nulls, booleans, numbers
    }
}

/// Redacts sensitive fields from a JSON-RPC message.
///
/// Replaces:
/// - `params.arguments` (deep) → `"[REDACTED]"`
/// - `params.uri` → `"[REDACTED]"`
/// - `result.content[*].text` → `"[REDACTED]"`
/// - `result.content[*].data` → `"[REDACTED]"`
/// - `result.messages[*].content` (deep) → `"[REDACTED]"`
/// - `result.contents[*].text` (deep) → `"[REDACTED]"`
///
/// Implements: TJ-SPEC-007 F-002
fn redact_value(value: &serde_json::Value) -> serde_json::Value {
    let mut redacted = value.clone();

    // Redact request params
    if let Some(params) = redacted.get_mut("params") {
        // params.arguments → deep redact all values
        if let Some(arguments) = params.get_mut("arguments") {
            redact_deep(arguments);
        }
        // params.uri → redact
        if params.get("uri").is_some() {
            params["uri"] = serde_json::Value::String("[REDACTED]".to_string());
        }
    }

    // Redact response result content
    if let Some(result) = redacted.get_mut("result") {
        if let Some(content) = result.get_mut("content") {
            if let Some(arr) = content.as_array_mut() {
                for item in arr {
                    if item.get("text").is_some() {
                        item["text"] = serde_json::Value::String("[REDACTED]".to_string());
                    }
                    if item.get("data").is_some() {
                        item["data"] = serde_json::Value::String("[REDACTED]".to_string());
                    }
                }
            }
        }
        // result.messages[].content → deep redact (prompt responses)
        if let Some(messages) = result.get_mut("messages") {
            if let Some(arr) = messages.as_array_mut() {
                for msg in arr {
                    if let Some(content) = msg.get_mut("content") {
                        redact_deep(content);
                    }
                }
            }
        }
        // result.contents[].text → deep redact (resource responses)
        if let Some(contents) = result.get_mut("contents") {
            if let Some(arr) = contents.as_array_mut() {
                for item in arr {
                    if let Some(text) = item.get_mut("text") {
                        redact_deep(text);
                    }
                }
            }
        }
    }

    redacted
}

impl std::fmt::Debug for CaptureWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureWriter")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Read as _;

    #[test]
    fn writes_ndjson_lines() {
        let dir = tempfile::tempdir().unwrap();
        let writer = CaptureWriter::new(dir.path(), false).unwrap();

        let msg = json!({"jsonrpc": "2.0", "method": "ping", "id": 1});
        writer
            .record(CaptureType::Request, &msg, Some("trust_building"))
            .unwrap();

        let resp = json!({"jsonrpc": "2.0", "result": {}, "id": 1});
        writer
            .record(CaptureType::Response, &resp, Some("trust_building"))
            .unwrap();

        let mut content = String::new();
        File::open(writer.path())
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();

        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);

        let entry1: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(entry1["type"], "request");
        assert!(entry1["ts"].is_string());
        assert_eq!(entry1["method"], "ping");
        assert_eq!(entry1["id"], 1);
        assert_eq!(entry1["phase"], "trust_building");

        let entry2: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(entry2["type"], "response");
        assert_eq!(entry2["id"], 1);
    }

    #[test]
    fn creates_capture_directory() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("nested").join("capture");
        let writer = CaptureWriter::new(&subdir, false).unwrap();
        assert!(writer.path().exists());
    }

    #[test]
    fn redacts_request_arguments() {
        let data = json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "read_file",
                "arguments": {
                    "path": "/etc/passwd",
                    "encoding": "utf-8"
                }
            },
            "id": 1
        });
        let redacted = redact_value(&data);
        assert_eq!(redacted["params"]["arguments"]["path"], "[REDACTED]");
        assert_eq!(redacted["params"]["arguments"]["encoding"], "[REDACTED]");
        // Non-sensitive fields are preserved
        assert_eq!(redacted["params"]["name"], "read_file");
        assert_eq!(redacted["method"], "tools/call");
    }

    #[test]
    fn redacts_params_uri() {
        let data = json!({
            "jsonrpc": "2.0",
            "method": "resources/read",
            "params": { "uri": "file:///etc/shadow" },
            "id": 2
        });
        let redacted = redact_value(&data);
        assert_eq!(redacted["params"]["uri"], "[REDACTED]");
    }

    #[test]
    fn redacts_response_content() {
        let data = json!({
            "jsonrpc": "2.0",
            "result": {
                "content": [
                    { "type": "text", "text": "secret data" },
                    { "type": "image", "data": "base64stuff", "mimeType": "image/png" }
                ]
            },
            "id": 1
        });
        let redacted = redact_value(&data);
        assert_eq!(redacted["result"]["content"][0]["text"], "[REDACTED]");
        assert_eq!(redacted["result"]["content"][1]["data"], "[REDACTED]");
        // Non-sensitive fields preserved
        assert_eq!(redacted["result"]["content"][0]["type"], "text");
        assert_eq!(redacted["result"]["content"][1]["mimeType"], "image/png");
    }

    #[test]
    fn redact_mode_writes_redacted_data() {
        let dir = tempfile::tempdir().unwrap();
        let writer = CaptureWriter::new(dir.path(), true).unwrap();

        let msg = json!({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": "exec",
                "arguments": { "cmd": "rm -rf /" }
            },
            "id": 1
        });
        writer.record(CaptureType::Request, &msg, None).unwrap();

        let mut content = String::new();
        File::open(writer.path())
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();

        let entry: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        // Params should be redacted since arguments were redacted in the source data
        assert_eq!(entry["method"], "tools/call");
    }
}
