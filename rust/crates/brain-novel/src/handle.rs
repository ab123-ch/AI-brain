use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::{
    MainReviewRecord, NovelBrainError, NovelBrainEvent, NovelBrainStatus, NovelConversationSource,
    NovelOutcome, NovelResumeInput, NovelTaskRequest, NovelTransition, PublicationReceipt, Result,
    UserDecisionRecord,
};

pub(crate) enum NovelCommand {
    StartTask {
        request: NovelTaskRequest,
        reply: oneshot::Sender<Result<NovelOutcome>>,
    },
    ResumeTask {
        task_id: String,
        input: NovelResumeInput,
        reply: oneshot::Sender<Result<NovelOutcome>>,
    },
    ReviewDraft {
        review: MainReviewRecord,
        reply: oneshot::Sender<Result<NovelTransition>>,
    },
    UserDecision {
        decision: UserDecisionRecord,
        reply: oneshot::Sender<Result<NovelTransition>>,
    },
    Publish {
        task_id: String,
        draft_version: u32,
        reply: oneshot::Sender<Result<PublicationReceipt>>,
    },
    Status {
        project_id: Option<String>,
        reply: oneshot::Sender<Result<NovelBrainStatus>>,
    },
    InvalidateConversation {
        conversation_id: String,
        generation_ids: Vec<String>,
        include_unscoped: bool,
        reply: oneshot::Sender<Result<Vec<String>>>,
    },
    AssociateConversationSource {
        task_id: String,
        source: NovelConversationSource,
        reply: oneshot::Sender<Result<()>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

#[derive(Clone)]
pub struct NovelBrainHandle {
    tx: mpsc::Sender<NovelCommand>,
    events: broadcast::Sender<NovelBrainEvent>,
    shutdown: watch::Sender<bool>,
}

impl NovelBrainHandle {
    pub(crate) fn new(
        tx: mpsc::Sender<NovelCommand>,
        events: broadcast::Sender<NovelBrainEvent>,
        shutdown: watch::Sender<bool>,
    ) -> Self {
        Self {
            tx,
            events,
            shutdown,
        }
    }

    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<NovelBrainEvent> {
        self.events.subscribe()
    }

    pub async fn start_task(&self, request: NovelTaskRequest) -> Result<NovelOutcome> {
        let (reply, receiver) = oneshot::channel();
        self.send(NovelCommand::StartTask { request, reply })?;
        receive(receiver).await
    }

    pub async fn resume_task(
        &self,
        task_id: impl Into<String>,
        input: NovelResumeInput,
    ) -> Result<NovelOutcome> {
        let (reply, receiver) = oneshot::channel();
        self.send(NovelCommand::ResumeTask {
            task_id: task_id.into(),
            input,
            reply,
        })?;
        receive(receiver).await
    }

    pub async fn review_draft(&self, review: MainReviewRecord) -> Result<NovelTransition> {
        let (reply, receiver) = oneshot::channel();
        self.send(NovelCommand::ReviewDraft { review, reply })?;
        receive(receiver).await
    }

    pub async fn user_decision(&self, decision: UserDecisionRecord) -> Result<NovelTransition> {
        let (reply, receiver) = oneshot::channel();
        self.send(NovelCommand::UserDecision { decision, reply })?;
        receive(receiver).await
    }

    pub async fn publish(
        &self,
        task_id: impl Into<String>,
        draft_version: u32,
    ) -> Result<PublicationReceipt> {
        let (reply, receiver) = oneshot::channel();
        self.send(NovelCommand::Publish {
            task_id: task_id.into(),
            draft_version,
            reply,
        })?;
        receive(receiver).await
    }

    pub async fn status(&self, project_id: Option<String>) -> Result<NovelBrainStatus> {
        let (reply, receiver) = oneshot::channel();
        self.send(NovelCommand::Status { project_id, reply })?;
        receive(receiver).await
    }

    pub async fn invalidate_conversation_generations(
        &self,
        conversation_id: impl Into<String>,
        generation_ids: Vec<String>,
        include_unscoped: bool,
    ) -> Result<Vec<String>> {
        let (reply, receiver) = oneshot::channel();
        self.send(NovelCommand::InvalidateConversation {
            conversation_id: conversation_id.into(),
            generation_ids,
            include_unscoped,
            reply,
        })?;
        receive(receiver).await
    }

    pub async fn associate_conversation_source(
        &self,
        task_id: impl Into<String>,
        source: NovelConversationSource,
    ) -> Result<()> {
        let (reply, receiver) = oneshot::channel();
        self.send(NovelCommand::AssociateConversationSource {
            task_id: task_id.into(),
            source,
            reply,
        })?;
        receive(receiver).await
    }

    pub async fn shutdown(&self) -> Result<()> {
        let (reply, receiver) = oneshot::channel();
        self.tx
            .send(NovelCommand::Shutdown { reply })
            .await
            .map_err(|_| NovelBrainError::Unavailable)?;
        receiver.await.map_err(|_| NovelBrainError::Unavailable)
    }

    pub fn request_shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    fn send(&self, command: NovelCommand) -> Result<()> {
        self.tx.try_send(command).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => NovelBrainError::Backpressure,
            mpsc::error::TrySendError::Closed(_) => NovelBrainError::Unavailable,
        })
    }
}

async fn receive<T>(receiver: oneshot::Receiver<Result<T>>) -> Result<T> {
    receiver.await.map_err(|_| NovelBrainError::Unavailable)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn request_shutdown_bypasses_full_command_queue() {
        let (tx, _rx) = mpsc::channel(1);
        let (events, _) = broadcast::channel(1);
        let (shutdown, mut shutdown_rx) = watch::channel(false);
        let filler = tx.clone();
        let handle = NovelBrainHandle::new(tx, events, shutdown);
        let (reply, _receiver) = oneshot::channel();
        assert!(filler
            .try_send(NovelCommand::Status {
                project_id: None,
                reply,
            })
            .is_ok());

        handle.request_shutdown();

        shutdown_rx.changed().await.unwrap();
        assert!(*shutdown_rx.borrow());
    }
}
