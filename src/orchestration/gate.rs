//! Readiness gate for server→client startup sequencing.
//!
//! Server-mode actors must be ready to accept connections before client-mode
//! actors start sending requests. `ReadinessGate` enforces this ordering
//! using oneshot channels (one per server) and a broadcast notification.
//!
//! See TJ-SPEC-015 §3.1 for the readiness gate specification.

use std::time::Duration;

use tokio::sync::{broadcast, oneshot};

// ============================================================================
// GateError
// ============================================================================

/// Errors from the readiness gate.
///
/// Implements: TJ-SPEC-015 F-002
#[derive(Debug)]
pub enum GateError {
    /// Timeout waiting for all servers to become ready.
    Timeout {
        /// Names of actors that did not signal readiness.
        not_ready: Vec<String>,
    },
    /// A server actor failed (dropped its oneshot sender) before signaling.
    ServerFailed {
        /// Name of the actor that failed.
        actor: String,
    },
}

impl std::fmt::Display for GateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout { not_ready } => {
                write!(f, "readiness timeout: not ready: {}", not_ready.join(", "))
            }
            Self::ServerFailed { actor } => {
                write!(f, "server actor '{actor}' failed before becoming ready")
            }
        }
    }
}

impl std::error::Error for GateError {}

// ============================================================================
// ReadinessGate
// ============================================================================

/// Coordinates server readiness before client startup.
///
/// Each server actor receives a `oneshot::Sender<()>` to signal readiness.
/// Client actors subscribe to a `broadcast::Receiver<()>` that fires once
/// all servers have signaled.
///
/// Implements: TJ-SPEC-015 F-002
pub struct ReadinessGate {
    /// `(actor_name, receiver)` for each server actor.
    ready_rxs: Vec<(String, oneshot::Receiver<()>)>,
    /// Broadcast sender — fires when all servers are ready.
    gate_tx: broadcast::Sender<()>,
}

impl ReadinessGate {
    /// Creates a new readiness gate for the given server actors.
    ///
    /// Returns the gate and a vec of `(actor_name, oneshot::Sender)` pairs.
    /// Each server actor must call `send(())` on its sender when ready.
    ///
    /// Implements: TJ-SPEC-015 F-002
    #[must_use]
    #[allow(clippy::similar_names)]
    pub fn new(server_actors: &[String]) -> (Self, Vec<(String, oneshot::Sender<()>)>) {
        let (gate_tx, _) = broadcast::channel(1);
        let mut receivers = Vec::with_capacity(server_actors.len());
        let mut senders = Vec::with_capacity(server_actors.len());

        for name in server_actors {
            let (tx, rx) = oneshot::channel();
            receivers.push((name.clone(), rx));
            senders.push((name.clone(), tx));
        }

        let gate = Self {
            ready_rxs: receivers,
            gate_tx,
        };
        (gate, senders)
    }

    /// Returns a broadcast receiver that fires when all servers are ready.
    ///
    /// Client actors should call this before spawning, then `await`
    /// the returned receiver before starting protocol I/O.
    ///
    /// Implements: TJ-SPEC-015 F-002
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.gate_tx.subscribe()
    }

    /// Waits for all server actors to signal readiness, with a timeout.
    ///
    /// On success, fires the broadcast so all subscribed clients unblock.
    /// On timeout or server failure, returns an error with the names
    /// of actors that did not become ready.
    ///
    /// # Errors
    ///
    /// Returns `GateError::Timeout` if the timeout expires before all
    /// servers signal. Returns `GateError::ServerFailed` if a server
    /// drops its sender without signaling.
    ///
    /// Implements: TJ-SPEC-015 F-002
    pub async fn wait_all_ready(self, timeout: Duration) -> Result<(), GateError> {
        let Self { ready_rxs, gate_tx } = self;
        let all_names: Vec<String> = ready_rxs.iter().map(|(n, _)| n.clone()).collect();
        let result = tokio::time::timeout(timeout, wait_all_receivers(ready_rxs)).await;

        match result {
            Ok(Ok(())) => {
                // All servers ready — fire the broadcast gate
                let _ = gate_tx.send(());
                Ok(())
            }
            Ok(Err(gate_err)) => Err(gate_err),
            Err(_elapsed) => Err(GateError::Timeout {
                not_ready: all_names,
            }),
        }
    }
}

/// Awaits all oneshot receivers sequentially.
async fn wait_all_receivers(
    ready_rxs: Vec<(String, oneshot::Receiver<()>)>,
) -> Result<(), GateError> {
    for (name, rx) in ready_rxs {
        rx.await
            .map_err(|_| GateError::ServerFailed { actor: name })?;
    }
    Ok(())
}

impl std::fmt::Debug for ReadinessGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadinessGate")
            .field("pending_servers", &self.ready_rxs.len())
            .finish_non_exhaustive()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn all_servers_ready_opens_gate() {
        let servers = vec!["server1".to_string(), "server2".to_string()];
        let (gate, ready_txs) = ReadinessGate::new(&servers);
        let mut client_rx = gate.subscribe();

        // Signal both servers
        for (_, tx) in ready_txs {
            tx.send(()).unwrap();
        }

        // Gate should open
        gate.wait_all_ready(Duration::from_secs(5)).await.unwrap();

        // Client should be unblocked
        assert!(client_rx.recv().await.is_ok());
    }

    #[tokio::test]
    async fn timeout_returns_error_with_server_names() {
        let servers = vec!["server1".to_string(), "server2".to_string()];
        let (gate, mut ready_txs) = ReadinessGate::new(&servers);

        // Only signal one server
        let (_, tx) = ready_txs.remove(0);
        tx.send(()).unwrap();
        // server2 never signals

        let result = gate.wait_all_ready(Duration::from_millis(50)).await;
        assert!(result.is_err());
        if let Err(GateError::Timeout { not_ready }) = result {
            // Should include the server names
            assert!(
                not_ready.contains(&"server1".to_string())
                    || not_ready.contains(&"server2".to_string()),
                "Expected server names in not_ready, got: {not_ready:?}"
            );
        } else {
            panic!("Expected GateError::Timeout, got {result:?}");
        }
    }

    #[tokio::test]
    async fn zero_servers_no_gate_needed() {
        let servers: Vec<String> = vec![];
        let (gate, ready_txs) = ReadinessGate::new(&servers);
        assert!(ready_txs.is_empty());

        // Should complete immediately (no receivers to wait on)
        gate.wait_all_ready(Duration::from_secs(1)).await.unwrap();
    }

    #[tokio::test]
    async fn dropped_sender_detected() {
        let servers = vec!["server1".to_string()];
        let (gate, ready_txs) = ReadinessGate::new(&servers);

        // Drop the sender without signaling
        drop(ready_txs);

        let result = gate.wait_all_ready(Duration::from_secs(1)).await;
        assert!(result.is_err());
        if let Err(GateError::ServerFailed { actor }) = result {
            assert_eq!(actor, "server1");
        } else {
            panic!("Expected GateError::ServerFailed, got {result:?}");
        }
    }

    #[tokio::test]
    async fn subscribe_before_ready() {
        let servers = vec!["server1".to_string()];
        let (gate, ready_txs) = ReadinessGate::new(&servers);
        let mut client_rx = gate.subscribe();

        // Client subscribes, then server signals
        let gate_handle =
            tokio::spawn(async move { gate.wait_all_ready(Duration::from_secs(5)).await });

        // Signal after a small delay
        let (_, tx) = ready_txs.into_iter().next().unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        tx.send(()).unwrap();

        gate_handle.await.unwrap().unwrap();
        assert!(client_rx.recv().await.is_ok());
    }
}
