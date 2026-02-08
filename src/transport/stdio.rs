//! Stdio transport implementation (TJ-SPEC-002 F-002).
//!
//! Implements the [`Transport`] trait for NDJSON (newline-delimited JSON)
//! communication over stdin/stdout, the standard MCP transport for local
//! development environments (Claude Desktop, Cursor, VS Code).

use super::{
    ConnectionContext, DEFAULT_MAX_MESSAGE_SIZE, DEFAULT_STDIO_BUFFER_SIZE, JsonRpcMessage, Result,
    Transport, TransportType,
};
use crate::config::schema::DeliveryConfig;

use std::str::FromStr;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::Mutex;

/// Configuration for the stdio transport.
///
/// Values are read from environment variables with fallback to defaults.
/// See TJ-SPEC-002 §5.1 for variable names and defaults.
///
/// Implements: TJ-SPEC-002 F-002
#[derive(Debug, Clone, Copy)]
pub struct StdioConfig {
    /// Maximum message size in bytes.
    pub max_message_size: usize,
    /// Read/write buffer size in bytes.
    pub buffer_size: usize,
}

impl StdioConfig {
    /// Loads configuration from environment variables with defaults.
    ///
    /// | Variable | Default |
    /// |----------|---------|
    /// | `THOUGHTJACK_MAX_MESSAGE_SIZE` | 10 MB |
    /// | `THOUGHTJACK_STDIO_BUFFER_SIZE` | 64 KB |
    ///
    /// Implements: TJ-SPEC-002 F-002
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            max_message_size: env_or("THOUGHTJACK_MAX_MESSAGE_SIZE", DEFAULT_MAX_MESSAGE_SIZE),
            buffer_size: env_or("THOUGHTJACK_STDIO_BUFFER_SIZE", DEFAULT_STDIO_BUFFER_SIZE),
        }
    }
}

impl Default for StdioConfig {
    fn default() -> Self {
        Self {
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
            buffer_size: DEFAULT_STDIO_BUFFER_SIZE,
        }
    }
}

/// NDJSON transport over stdin/stdout.
///
/// Uses separate `tokio::sync::Mutex` locks for reader and writer to allow
/// concurrent read and write operations. The async mutex is required because
/// the lock is held across `.await` points.
///
/// # Edge Cases Handled
///
/// - **EC-TRANS-008**: Last line without `\n` — `read_line` returns content,
///   next call returns 0 bytes (EOF).
/// - **EC-TRANS-009**: Empty lines — trimmed empty lines are skipped.
/// - **EC-TRANS-016**: Multiple JSON objects on one line — parse error, logged and skipped.
/// - **F-008**: Message size limit — checked after read, oversized messages are logged and skipped.
///
/// Implements: TJ-SPEC-002 F-002
pub struct StdioTransport {
    reader: Mutex<BufReader<tokio::io::Stdin>>,
    writer: Mutex<BufWriter<tokio::io::Stdout>>,
    config: StdioConfig,
    context: ConnectionContext,
}

impl StdioTransport {
    /// Creates a new stdio transport with configuration from environment variables.
    ///
    /// Implements: TJ-SPEC-002 F-002
    #[must_use]
    pub fn new() -> Self {
        let config = StdioConfig::from_env();
        Self {
            reader: Mutex::new(BufReader::with_capacity(
                config.buffer_size,
                tokio::io::stdin(),
            )),
            writer: Mutex::new(BufWriter::with_capacity(
                config.buffer_size,
                tokio::io::stdout(),
            )),
            config,
            context: ConnectionContext::stdio(),
        }
    }

    /// Creates a new stdio transport with explicit configuration.
    ///
    /// Primarily used in tests and by library consumers embedding the
    /// transport directly.
    ///
    /// Implements: TJ-SPEC-002 F-002
    #[must_use]
    pub fn with_config(config: StdioConfig) -> Self {
        Self {
            reader: Mutex::new(BufReader::with_capacity(
                config.buffer_size,
                tokio::io::stdin(),
            )),
            writer: Mutex::new(BufWriter::with_capacity(
                config.buffer_size,
                tokio::io::stdout(),
            )),
            config,
            context: ConnectionContext::stdio(),
        }
    }

    /// Returns a reference to the connection context.
    ///
    /// Primarily used in tests and by library consumers.
    ///
    /// Implements: TJ-SPEC-002 F-016
    #[must_use]
    pub const fn context(&self) -> &ConnectionContext {
        &self.context
    }
}

impl Default for StdioTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for StdioTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StdioTransport")
            .field("config", &self.config)
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl Transport for StdioTransport {
    async fn send_message(&self, message: &JsonRpcMessage) -> Result<()> {
        let serialized = serde_json::to_string(message)?;
        let mut writer = self.writer.lock().await;
        writer.write_all(serialized.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        drop(writer);
        Ok(())
    }

    async fn send_raw(&self, bytes: &[u8]) -> Result<()> {
        let mut writer = self.writer.lock().await;
        writer.write_all(bytes).await?;
        writer.flush().await?;
        drop(writer);
        Ok(())
    }

    #[allow(clippy::significant_drop_tightening)] // reader must be held across the loop
    async fn receive_message(&self) -> Result<Option<JsonRpcMessage>> {
        let mut reader = self.reader.lock().await;
        // Bounded line reading: prevents OOM from a single line without '\n'.
        // We read from the BufReader's internal buffer in a loop, copying up to
        // max_message_size + 1 bytes. If the line exceeds the limit before we
        // find '\n', we drain the remainder and skip.
        let read_limit = self.config.max_message_size + 1;
        let mut buf: Vec<u8> = Vec::with_capacity(read_limit.min(64 * 1024));

        loop {
            buf.clear();
            let mut overflowed = false;

            // Bounded line read using fill_buf + consume
            loop {
                let available = reader.fill_buf().await?;
                if available.is_empty() {
                    // EOF
                    if buf.is_empty() {
                        return Ok(None);
                    }
                    // Last line without trailing '\n' (EC-TRANS-008)
                    break;
                }

                // Find newline in available buffer
                if let Some(pos) = available.iter().position(|&b| b == b'\n') {
                    if !overflowed {
                        let remaining_cap = read_limit.saturating_sub(buf.len());
                        let copy_len = pos.min(remaining_cap);
                        buf.extend_from_slice(&available[..copy_len]);
                        if pos > remaining_cap {
                            overflowed = true;
                        }
                    }
                    reader.consume(pos + 1); // consume through the newline
                    break;
                }

                // No newline in this chunk — append if within limit
                if !overflowed {
                    let remaining_cap = read_limit.saturating_sub(buf.len());
                    if remaining_cap == 0 {
                        overflowed = true;
                    } else {
                        let copy_len = available.len().min(remaining_cap);
                        buf.extend_from_slice(&available[..copy_len]);
                        if available.len() > remaining_cap {
                            overflowed = true;
                        }
                    }
                }
                let consumed = available.len();
                reader.consume(consumed);
            }

            if overflowed {
                tracing::warn!(
                    limit = self.config.max_message_size,
                    "message exceeds size limit (read capped), skipping"
                );
                continue;
            }

            let line = match std::str::from_utf8(&buf) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("invalid UTF-8 in message, skipping line: {e}");
                    continue;
                }
            };
            let trimmed = line.trim();

            // EC-TRANS-009: Skip empty lines
            if trimmed.is_empty() {
                continue;
            }

            // Parse JSON-RPC message (EC-TRANS-016: invalid NDJSON logged and skipped)
            match serde_json::from_str::<JsonRpcMessage>(trimmed) {
                Ok(message) => return Ok(Some(message)),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        line = %sanitize_for_log(trimmed, 200),
                        "invalid JSON-RPC message, skipping"
                    );
                }
            }
        }
    }

    fn supports_behavior(&self, _behavior: &DeliveryConfig) -> bool {
        // stdio supports all delivery behaviors
        true
    }

    fn transport_type(&self) -> TransportType {
        TransportType::Stdio
    }

    async fn finalize_response(&self) -> Result<()> {
        // No-op for stdio — responses are complete after send_message/send_raw.
        Ok(())
    }

    fn connection_context(&self) -> ConnectionContext {
        self.context.clone()
    }
}

/// Truncates and strips control characters from untrusted input before logging.
///
/// Replaces control characters (except tab) with the Unicode replacement
/// character to prevent log injection attacks via raw stdio input.
fn sanitize_for_log(input: &str, max_len: usize) -> String {
    input
        .chars()
        .take(max_len)
        .map(|c| {
            if c.is_control() && c != '\t' {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}

/// Reads an environment variable, parsing it to type `T`, or returns the default.
///
/// Logs a warning if the variable is set but cannot be parsed.
fn env_or<T: FromStr>(name: &str, default: T) -> T {
    match std::env::var(name) {
        Ok(v) => v.parse().unwrap_or_else(|_| {
            tracing::warn!(name, value = %v, "invalid env var value, using default");
            default
        }),
        Err(_) => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stdio_config_default() {
        let config = StdioConfig::default();
        assert_eq!(config.max_message_size, DEFAULT_MAX_MESSAGE_SIZE);
        assert_eq!(config.buffer_size, DEFAULT_STDIO_BUFFER_SIZE);
    }

    #[test]
    fn test_env_or_default() {
        // With a non-existent env var, should return default
        let result: usize = env_or("THOUGHTJACK_TEST_NONEXISTENT_VAR_12345", 42);
        assert_eq!(result, 42);
    }

    #[test]
    fn test_stdio_transport_debug() {
        let transport = StdioTransport::new();
        let debug = format!("{transport:?}");
        assert!(debug.contains("StdioTransport"));
        assert!(debug.contains("config"));
    }

    #[test]
    fn test_stdio_transport_type() {
        let transport = StdioTransport::new();
        assert_eq!(transport.transport_type(), TransportType::Stdio);
    }

    #[test]
    fn test_stdio_supports_all_behaviors() {
        let transport = StdioTransport::new();
        assert!(transport.supports_behavior(&DeliveryConfig::Normal));
        assert!(transport.supports_behavior(&DeliveryConfig::SlowLoris {
            byte_delay_ms: Some(100),
            chunk_size: Some(1),
        }));
        assert!(transport.supports_behavior(&DeliveryConfig::UnboundedLine {
            target_bytes: Some(1000),
            padding_char: None,
        }));
        assert!(transport.supports_behavior(&DeliveryConfig::NestedJson {
            depth: 100,
            key: None,
        }));
        assert!(transport.supports_behavior(&DeliveryConfig::ResponseDelay { delay_ms: 1000 }));
    }

    #[test]
    fn test_stdio_context() {
        let transport = StdioTransport::new();
        let ctx = transport.context();
        assert_eq!(ctx.connection_id, 0);
        assert!(ctx.remote_addr.is_none());
        assert!(ctx.is_exclusive);
    }
}
