use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::{
    AdmissionLease, AdmissionRequest, Result, Scheduler, StartedNode, TaskEngineError,
    TaskRepository,
};

#[derive(Clone)]
pub struct TaskCoordinator {
    repository: Arc<TaskRepository>,
    scheduler: Scheduler,
}

impl TaskCoordinator {
    #[must_use]
    pub fn new(repository: Arc<TaskRepository>, scheduler: Scheduler) -> Self {
        Self {
            repository,
            scheduler,
        }
    }

    #[must_use]
    pub fn repository(&self) -> &Arc<TaskRepository> {
        &self.repository
    }

    #[must_use]
    pub fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    pub async fn admit_node(
        &self,
        node_id: &str,
        instance_run_id: &str,
        cancellation: CancellationToken,
    ) -> Result<CoordinatedNode> {
        let repository = Arc::clone(&self.repository);
        let node_for_load = node_id.to_string();
        let node = tokio::task::spawn_blocking(move || repository.node(&node_for_load))
            .await
            .map_err(|error| TaskEngineError::CoordinatorWorker(error.to_string()))??;
        let admission = self
            .scheduler
            .admit(
                AdmissionRequest {
                    request_id: instance_run_id.into(),
                    task_run_id: node.task_run_id.clone(),
                    room_id: node.room_id.clone(),
                    member_id: node.member_id.clone(),
                    provider: node.provider.clone(),
                    profile: node.profile.clone(),
                },
                cancellation,
            )
            .await?;

        // The node may have changed while admission was queued. Reloading here
        // makes the following short transaction use the authoritative version.
        let repository = Arc::clone(&self.repository);
        let node_for_start = node_id.to_string();
        let run_for_start = instance_run_id.to_string();
        let start_result = tokio::task::spawn_blocking(move || {
            let current = repository.node(&node_for_start)?;
            repository.start_node(&node_for_start, current.version, &run_for_start)
        })
        .await
        .map_err(|error| TaskEngineError::CoordinatorWorker(error.to_string()))?;
        let started = match start_result {
            Ok(started) => started,
            Err(error @ TaskEngineError::BudgetExceeded { .. }) => {
                let repository = Arc::clone(&self.repository);
                let task_run_id = node.task_run_id.clone();
                let reason = error.to_string();
                tokio::task::spawn_blocking(move || {
                    repository.pause_task_for_budget(&task_run_id, &reason)
                })
                .await
                .map_err(|join_error| {
                    TaskEngineError::CoordinatorWorker(join_error.to_string())
                })??;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        Ok(CoordinatedNode { started, admission })
    }
}

pub struct CoordinatedNode {
    started: StartedNode,
    admission: AdmissionLease,
}

impl CoordinatedNode {
    #[must_use]
    pub fn started(&self) -> &StartedNode {
        &self.started
    }

    #[must_use]
    pub fn into_parts(self) -> (StartedNode, AdmissionLease) {
        (self.started, self.admission)
    }
}
