//! Traffic capture module (TJ-SPEC-007).
//!
//! Captures request/response traffic to NDJSON files for analysis.
//! Each session creates a `session-<timestamp>.jsonl` file in the
//! configured capture directory.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tracing::debug;

use crate::error::ThoughtJackError;

/// Direction of the captured message.
///
/// Implements: TJ-SPEC-007 F-002
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureDirection {
    /// Request from client to server.
    Request,
    /// Response from server to client.
    Response,
}

/// A single captured traffic entry.
///
/// Implements: TJ-SPEC-007 F-002
#[derive(Debug, Serialize)]
struct CaptureEntry<'a> {
    /// ISO 8601 timestamp.
    timestamp: String,
    /// Request or response.
    direction: CaptureDirection,
    /// The raw JSON-RPC message data.
    data: &'a serde_json::Value,
}

/// Writer for traffic capture files.
///
/// Writes NDJSON (newline-delimited JSON) lines to a session file.
/// Thread-safe via internal `Mutex`.
///
/// Implements: TJ-SPEC-007 F-002
pub struct CaptureWriter {
    writer: Mutex<BufWriter<File>>,
    path: PathBuf,
}

impl CaptureWriter {
    /// Creates a new capture writer in the given directory.
    ///
    /// Creates the directory if it doesn't exist and opens a new session
    /// file named `session-<timestamp>.jsonl`.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created or the file
    /// cannot be opened.
    ///
    /// Implements: TJ-SPEC-007 F-002
    pub fn new(capture_dir: &Path) -> Result<Self, ThoughtJackError> {
        fs::create_dir_all(capture_dir)?;

        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let filename = format!("session-{timestamp}.jsonl");
        let path = capture_dir.join(filename);

        let file = OpenOptions::new().create(true).append(true).open(&path)?;

        debug!(path = %path.display(), "capture file opened");

        Ok(Self {
            writer: Mutex::new(BufWriter::new(file)),
            path,
        })
    }

    /// Records a message in the capture file.
    ///
    /// Serializes the entry as a single NDJSON line.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization or I/O fails.
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned.
    ///
    /// Implements: TJ-SPEC-007 F-002
    pub fn record(
        &self,
        direction: CaptureDirection,
        data: &serde_json::Value,
    ) -> Result<(), ThoughtJackError> {
        let entry = CaptureEntry {
            timestamp: chrono::Utc::now().to_rfc3339(),
            direction,
            data,
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
        let writer = CaptureWriter::new(dir.path()).unwrap();

        let msg = json!({"jsonrpc": "2.0", "method": "ping"});
        writer.record(CaptureDirection::Request, &msg).unwrap();

        let resp = json!({"jsonrpc": "2.0", "result": {}, "id": 1});
        writer.record(CaptureDirection::Response, &resp).unwrap();

        let mut content = String::new();
        File::open(writer.path())
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();

        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2);

        let entry1: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(entry1["direction"], "request");
        assert!(entry1["timestamp"].is_string());
        assert_eq!(entry1["data"]["method"], "ping");

        let entry2: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(entry2["direction"], "response");
    }

    #[test]
    fn creates_capture_directory() {
        let dir = tempfile::tempdir().unwrap();
        let subdir = dir.path().join("nested").join("capture");
        let writer = CaptureWriter::new(&subdir).unwrap();
        assert!(writer.path().exists());
    }
}
