use std::sync::Arc;

use tokio::sync::{mpsc, Mutex, watch};

use crate::types::{DispatchEvent, PrioritizedEvent, Priority};

/// Tokio-based dispatch bus for routing events between agents and brains.
#[derive(Clone)]
pub struct TokioDispatch {
    queue_tx: mpsc::Sender<PrioritizedEvent>,
    queue_rx: Arc<Mutex<mpsc::Receiver<PrioritizedEvent>>>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl TokioDispatch {
    /// Create a new dispatch bus with the given queue capacity.
    pub fn new(capacity: usize) -> Self {
        let (queue_tx, queue_rx) = mpsc::channel(capacity);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        Self {
            queue_tx,
            queue_rx: Arc::new(Mutex::new(queue_rx)),
            shutdown_tx,
            shutdown_rx,
        }
    }

    /// Inject an event into the dispatch queue with the given priority.
    pub async fn inject(&self, event: DispatchEvent, priority: Priority) {
        let prioritized = PrioritizedEvent {
            event,
            priority,
            enqueued_at: std::time::Instant::now(),
        };

        // If the queue is full we drop the event on the floor.
        // A more sophisticated implementation could return an error or apply
        // back-pressure, but for the skeleton this is sufficient.
        let _ = self.queue_tx.send(prioritized).await;
    }

    /// Signal the dispatch bus to shut down.
    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Receive the next prioritized event from the queue.
    pub async fn recv(&self) -> Option<PrioritizedEvent> {
        let mut rx = self.queue_rx.lock().await;
        rx.recv().await
    }

    /// Check whether a shutdown has been requested.
    pub fn is_shutdown(&self) -> bool {
        *self.shutdown_rx.borrow()
    }
}
