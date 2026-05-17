use std::sync::Arc;

use tokio::sync::{mpsc, Mutex, watch};

use crate::types::{DispatchEvent, MainLoopMessage, PrioritizedEvent, Priority};

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

    /// Launch the main dispatch loop — consumes the priority queue and routes
    /// events to `output_tx`.
    pub async fn run_dispatch_loop(
        &self,
        output_tx: mpsc::Sender<MainLoopMessage>,
    ) {
        let mut rx = self.queue_rx.lock().await;

        loop {
            if *self.shutdown_rx.borrow() {
                tracing::info!("dispatch_loop: shutdown signal received");
                break;
            }

            // Wait for the next event
            match rx.recv().await {
                Some(pe) => {
                    // Handle this event
                    self.handle_event(pe.event, &output_tx).await;
                }
                None => {
                    tracing::info!("dispatch_loop: queue channel closed");
                    break;
                }
            }
        }
    }

    async fn handle_event(
        &self,
        event: DispatchEvent,
        output_tx: &mpsc::Sender<MainLoopMessage>,
    ) {
        match event {
            DispatchEvent::SyncAgentRequest {
                agent_id,
                reply_tx,
                ..
            } => {
                // Sync requests are handled by spawn_agent_job in a separate
                // thread; the reply is sent from that thread.
                // dispatch_loop drops the reply_tx here as a no-op.
                tracing::debug!(
                    "dispatch_loop: SyncAgentRequest for {agent_id} (handled by caller)"
                );
                drop(reply_tx);
            }
            DispatchEvent::AsyncAgentCompleted(agent_result) => {
                let name = agent_result.agent_id.clone();
                let status = agent_result.status.clone();
                tracing::info!("dispatch_loop: AsyncAgentCompleted {name} status={status:?}");
                let msg = MainLoopMessage::AgentNotification(agent_result);
                if let Err(e) = output_tx.send(msg).await {
                    tracing::error!(
                        "dispatch_loop: failed to forward agent notification: {e}"
                    );
                }
            }
            DispatchEvent::BrainTaskCompleted { brain_id, result } => {
                let task_type = result.output.clone();
                tracing::info!("dispatch_loop: BrainTaskCompleted {brain_id} {task_type}");
                let msg = MainLoopMessage::BrainTaskNotification { brain_id, result };
                if let Err(e) = output_tx.send(msg).await {
                    tracing::error!(
                        "dispatch_loop: failed to forward brain notification: {e}"
                    );
                }
            }
            DispatchEvent::UserInput { content } => {
                tracing::debug!("dispatch_loop: UserInput ({} chars)", content.len());
            }
        }
    }

    /// Inject an async-agent completion event from a synchronous (std::thread)
    /// context.  Uses `try_send` so the caller does not need a tokio runtime.
    pub fn inject_sync(
        &self,
        agent_id: String,
        _name: String,
        status: crate::types::AgentStatus,
        output: String,
        error: Option<String>,
        duration_ms: u64,
    ) {
        let agent_result = crate::types::AgentResult {
            agent_id,
            status,
            output,
            error,
            duration_ms,
        };
        let event = DispatchEvent::AsyncAgentCompleted(agent_result);
        let pe = PrioritizedEvent::new(event, Priority::Background);

        match self.queue_tx.try_send(pe) {
            Ok(()) => {
                tracing::info!("async agent completion injected to dispatch queue")
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                tracing::error!("dispatch queue full, async agent completion dropped");
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                tracing::error!("dispatch queue closed, async agent completion dropped");
            }
        }
    }
}
