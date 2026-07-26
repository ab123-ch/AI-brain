//! Bounded execution runtime for durable collaboration members.
//!
//! Durable identity and queue state live in `CollaborationRepository`. This
//! runtime owns only worker permits, active cancellation tokens, and broadcast
//! delivery. No database connection or model object survives a single call.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use brain_llm::config::{LlmConfig, ResolvedModelPolicy};
use brain_memory::conversation_memory::ConversationMemoryScope;
use knowledge_core::{
    sha256_hex, ContextBlockInput, ContextBlockKind, ContextBudget, ContextRequest,
    ContextSnapshot, NamespaceId, ResourceTypeId, ScopeRef, ScopeTypeId, SourceRef, TenantId,
};
use task_engine::{
    ActualUsage, AdmissionLease, BudgetLimits, BudgetRequest, InstanceRunState, NewTaskNode,
    NewTaskRun, NodeKind, StartedNode, TaskCoordinator, TaskEngineError, TaskRepository, TaskRun,
    TaskRunState,
};
use tokio::sync::{broadcast, Mutex, Notify};
use tokio_util::sync::CancellationToken;

use crate::orchestrator::Orchestrator;
use crate::web::collaboration::{
    BrainMemberView, ClaimedInboxItem, CollaborationActor, CollaborationConfig, CollaborationError,
    CollaborationRepository, InboxPurpose, LegacyMessageSeed, MemberAddress, MemberHistoryMessage,
    ParticipationCompletion, ParticipationDisposition, PostMessageResult, RoomEventView,
    RoomInputMode, RoomSnapshot,
};
use crate::web::progress_adapter::WebProgressEvent;

const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(500);
const EVENT_CHANNEL_CAPACITY: usize = 1_024;

#[derive(Clone)]
struct ActiveRun {
    room_id: String,
    member_id: String,
    cancel: CancellationToken,
}

#[derive(Debug, Clone, Copy)]
enum FailedRunAccounting {
    PreExecution,
    RuntimeUsage(ActualUsage),
    ReservationUpperBound,
}

enum MemberCompletion {
    Published(Box<RoomEventView>),
    Silent,
    Suppressed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MemberExecutionPolicy {
    model_policy: String,
    reasoning_depth: String,
    allow_tools: bool,
}

#[derive(Debug)]
struct ClaimRunError {
    message: String,
    accounting: FailedRunAccounting,
}

impl ClaimRunError {
    fn pre_execution(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            accounting: FailedRunAccounting::PreExecution,
        }
    }

    fn after_execution(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            accounting: FailedRunAccounting::ReservationUpperBound,
        }
    }

    fn with_runtime_usage(message: impl Into<String>, usage: ActualUsage) -> Self {
        Self {
            message: message.into(),
            accounting: FailedRunAccounting::RuntimeUsage(usage),
        }
    }
}

impl std::fmt::Display for ClaimRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

pub struct CollaborationRuntime {
    repository: Arc<CollaborationRepository>,
    task_repository: Arc<TaskRepository>,
    coordinator: TaskCoordinator,
    orchestrator: Arc<Orchestrator>,
    events: broadcast::Sender<WebProgressEvent>,
    dispatcher_notify: Arc<Notify>,
    active_runs: Arc<Mutex<HashMap<String, ActiveRun>>>,
    model_policy_details: Vec<ResolvedModelPolicy>,
}

impl CollaborationRuntime {
    pub async fn start(
        repository: Arc<CollaborationRepository>,
        orchestrator: Arc<Orchestrator>,
    ) -> Result<Arc<Self>, String> {
        let task_repository = orchestrator.task_repository();
        let coordinator = orchestrator.task_coordinator();

        let result_tasks = Arc::clone(&task_repository);
        let durable_results =
            tokio::task::spawn_blocking(move || result_tasks.completed_results("member_inbox"))
                .await
                .map_err(|error| format!("读取任务完成事件线程失败: {error}"))?
                .map_err(|error| format!("读取任务完成事件失败: {error}"))?;
        let mut reconciled = 0_usize;
        for result in durable_results {
            let result_repository = Arc::clone(&repository);
            let projected = tokio::task::spawn_blocking(move || {
                reconcile_durable_result(
                    &result_repository,
                    &result.origin_id,
                    &result.instance_run_id,
                    &result.artifact.content,
                )
            })
            .await
            .map_err(|error| format!("恢复成员完成事件线程失败: {error}"))?
            .map_err(|error| format!("恢复成员完成事件失败: {error}"))?;
            reconciled += usize::from(projected.is_some());
        }
        if reconciled > 0 {
            tracing::warn!(reconciled, "已从持久 Task event 补齐成员回复");
        }

        let recovery_repository = Arc::clone(&repository);
        let recovered = tokio::task::spawn_blocking(move || recovery_repository.recover_inflight())
            .await
            .map_err(|error| format!("恢复协作任务失败: {error}"))?
            .map_err(|error| error.to_string())?;
        if recovered > 0 {
            tracing::warn!("已将 {recovered} 个中断的成员任务重新排队");
        }

        let (events, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let llm_config = LlmConfig::load_default().unwrap_or_else(|error| {
            tracing::warn!("解析协作模型展示信息失败，使用默认 LLM 配置: {error}");
            LlmConfig::default_config()
        });
        let model_policy_details =
            resolve_model_policy_details(&repository.config().allowed_model_policies, &llm_config);
        let runtime = Arc::new(Self {
            repository,
            task_repository,
            coordinator,
            orchestrator,
            events,
            dispatcher_notify: Arc::new(Notify::new()),
            active_runs: Arc::new(Mutex::new(HashMap::new())),
            model_policy_details,
        });
        let dispatcher = Arc::clone(&runtime);
        tokio::spawn(async move {
            dispatcher.dispatch_loop().await;
        });
        runtime.dispatcher_notify.notify_one();
        Ok(runtime)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WebProgressEvent> {
        self.events.subscribe()
    }

    pub async fn ensure_room(
        &self,
        room_id: String,
        title: String,
        legacy_messages: Vec<LegacyMessageSeed>,
    ) -> Result<RoomSnapshot, String> {
        let repository = Arc::clone(&self.repository);
        let snapshot = tokio::task::spawn_blocking(move || {
            repository.ensure_room(&room_id, &title, &legacy_messages)
        })
        .await
        .map_err(|error| format!("创建协作房间失败: {error}"))?
        .map_err(|error| error.to_string())?;
        Ok(self.with_model_policy_details(snapshot))
    }

    pub async fn snapshot(&self, room_id: String) -> Result<RoomSnapshot, String> {
        let repository = Arc::clone(&self.repository);
        let snapshot = tokio::task::spawn_blocking(move || repository.snapshot(&room_id))
            .await
            .map_err(|error| format!("读取协作房间失败: {error}"))?
            .map_err(|error| error.to_string())?;
        Ok(self.with_model_policy_details(snapshot))
    }

    pub async fn events_after(
        &self,
        room_id: String,
        after_sequence: u64,
    ) -> Result<Vec<RoomEventView>, String> {
        let repository = Arc::clone(&self.repository);
        tokio::task::spawn_blocking(move || {
            repository.events_after(&room_id, after_sequence, EVENT_CHANNEL_CAPACITY)
        })
        .await
        .map_err(|error| format!("重放协作房间事件失败: {error}"))?
        .map_err(|error| error.to_string())
    }

    fn with_model_policy_details(&self, mut snapshot: RoomSnapshot) -> RoomSnapshot {
        snapshot
            .model_policy_details
            .clone_from(&self.model_policy_details);
        snapshot
    }

    pub async fn post_message(
        &self,
        room_id: String,
        recipient_ids: Vec<String>,
        content: String,
        mode: RoomInputMode,
        command_id: String,
    ) -> Result<PostMessageResult, String> {
        let repository = Arc::clone(&self.repository);
        let room_for_commit = room_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            repository.post_message(
                &room_for_commit,
                &recipient_ids,
                &content,
                mode,
                &command_id,
            )
        })
        .await
        .map_err(|error| format!("发送群聊消息失败: {error}"))?
        .map_err(|error| error.to_string())?;

        if !result.duplicate {
            self.broadcast(WebProgressEvent::RoomEventAppended {
                event: result.event.clone(),
            });
            for item in &result.inbox_items {
                self.broadcast(WebProgressEvent::InboxItemChanged {
                    room_id: room_id.clone(),
                    item: item.clone(),
                });
            }
        }
        self.publish_snapshot(room_id).await;
        self.dispatcher_notify.notify_waiters();
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn post_message_checked(
        &self,
        room_id: String,
        recipients: Vec<MemberAddress>,
        content: String,
        mode: RoomInputMode,
        thread_key: String,
        expected_room_version: u64,
        command_id: String,
    ) -> Result<PostMessageResult, String> {
        let repository = Arc::clone(&self.repository);
        let room_for_commit = room_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            repository.post_group_message_checked(
                &CollaborationActor::local(),
                &room_for_commit,
                &recipients,
                &content,
                mode,
                &thread_key,
                expected_room_version,
                &command_id,
            )
        })
        .await
        .map_err(|error| format!("发送群聊消息失败: {error}"))?
        .map_err(|error| error.to_string())?;

        if !result.duplicate {
            self.broadcast(WebProgressEvent::RoomEventAppended {
                event: result.event.clone(),
            });
            for item in &result.inbox_items {
                self.broadcast(WebProgressEvent::InboxItemChanged {
                    room_id: room_id.clone(),
                    item: item.clone(),
                });
            }
        }
        self.publish_snapshot(room_id).await;
        self.dispatcher_notify.notify_waiters();
        Ok(result)
    }

    pub async fn create_member(
        &self,
        room_id: String,
        display_name: String,
        model_policy: Option<String>,
        reasoning_depth: Option<String>,
    ) -> Result<BrainMemberView, String> {
        let repository = Arc::clone(&self.repository);
        let room_for_commit = room_id.clone();
        let member = tokio::task::spawn_blocking(move || {
            repository.create_member(
                &room_for_commit,
                &display_name,
                model_policy.as_deref(),
                reasoning_depth.as_deref(),
            )
        })
        .await
        .map_err(|error| format!("创建成员失败: {error}"))?
        .map_err(|error| error.to_string())?;
        self.member_changed(room_id, member.clone()).await;
        Ok(member)
    }

    pub async fn configure_member(
        &self,
        room_id: String,
        member_id: String,
        display_name: String,
        model_policy: String,
        reasoning_depth: String,
        expected_version: u64,
    ) -> Result<BrainMemberView, String> {
        let repository = Arc::clone(&self.repository);
        let room_for_commit = room_id.clone();
        let member = tokio::task::spawn_blocking(move || {
            repository.configure_member(
                &room_for_commit,
                &member_id,
                &display_name,
                &model_policy,
                &reasoning_depth,
                expected_version,
            )
        })
        .await
        .map_err(|error| format!("更新成员失败: {error}"))?
        .map_err(|error| error.to_string())?;
        self.member_changed(room_id, member.clone()).await;
        Ok(member)
    }

    pub async fn wake_member(
        &self,
        room_id: String,
        member_id: String,
    ) -> Result<BrainMemberView, String> {
        let repository = Arc::clone(&self.repository);
        let room_for_commit = room_id.clone();
        let member = tokio::task::spawn_blocking(move || {
            repository.wake_member(&room_for_commit, &member_id)
        })
        .await
        .map_err(|error| format!("唤醒成员失败: {error}"))?
        .map_err(|error| error.to_string())?;
        self.member_changed(room_id, member.clone()).await;
        self.dispatcher_notify.notify_waiters();
        Ok(member)
    }

    pub async fn wake_member_checked(
        &self,
        room_id: String,
        member_id: String,
        expected_version: u64,
    ) -> Result<BrainMemberView, String> {
        let repository = Arc::clone(&self.repository);
        let room_for_commit = room_id.clone();
        let member = tokio::task::spawn_blocking(move || {
            repository.wake_member_checked(
                &CollaborationActor::local(),
                &room_for_commit,
                &member_id,
                expected_version,
            )
        })
        .await
        .map_err(|error| format!("唤醒成员失败: {error}"))?
        .map_err(|error| error.to_string())?;
        self.member_changed(room_id, member.clone()).await;
        self.dispatcher_notify.notify_waiters();
        Ok(member)
    }

    pub async fn sleep_member(
        &self,
        room_id: String,
        member_id: String,
    ) -> Result<BrainMemberView, String> {
        self.change_availability(room_id, member_id, AvailabilityCommand::Sleep)
            .await
    }

    pub async fn sleep_member_checked(
        &self,
        room_id: String,
        member_id: String,
        expected_version: u64,
    ) -> Result<BrainMemberView, String> {
        self.change_availability_checked(
            room_id,
            member_id,
            expected_version,
            AvailabilityCommand::Sleep,
        )
        .await
    }

    pub async fn archive_member(
        &self,
        room_id: String,
        member_id: String,
    ) -> Result<BrainMemberView, String> {
        self.change_availability(room_id, member_id, AvailabilityCommand::Archive)
            .await
    }

    pub async fn archive_member_checked(
        &self,
        room_id: String,
        member_id: String,
        expected_version: u64,
    ) -> Result<BrainMemberView, String> {
        self.change_availability_checked(
            room_id,
            member_id,
            expected_version,
            AvailabilityCommand::Archive,
        )
        .await
    }

    pub async fn restore_member(
        &self,
        room_id: String,
        member_id: String,
    ) -> Result<BrainMemberView, String> {
        let member = self
            .change_availability(room_id, member_id, AvailabilityCommand::Restore)
            .await?;
        self.dispatcher_notify.notify_waiters();
        Ok(member)
    }

    pub async fn restore_member_checked(
        &self,
        room_id: String,
        member_id: String,
        expected_version: u64,
    ) -> Result<BrainMemberView, String> {
        let member = self
            .change_availability_checked(
                room_id,
                member_id,
                expected_version,
                AvailabilityCommand::Restore,
            )
            .await?;
        self.dispatcher_notify.notify_waiters();
        Ok(member)
    }

    pub async fn interrupt_run(
        &self,
        room_id: String,
        member_id: String,
        run_id: String,
    ) -> Result<(), String> {
        let repository = Arc::clone(&self.repository);
        let room_for_commit = room_id.clone();
        let member_for_commit = member_id.clone();
        let run_for_commit = run_id.clone();
        tokio::task::spawn_blocking(move || {
            repository.request_interrupt(&room_for_commit, &member_for_commit, &run_for_commit)
        })
        .await
        .map_err(|error| format!("中断成员运行失败: {error}"))?
        .map_err(|error| error.to_string())?;

        self.cancel_task_and_runtime(&room_id, &member_id, &run_id)
            .await;
        self.publish_snapshot(room_id).await;
        Ok(())
    }

    pub async fn interrupt_run_checked(
        &self,
        room_id: String,
        member_id: String,
        run_id: String,
        expected_version: u64,
    ) -> Result<(), String> {
        let repository = Arc::clone(&self.repository);
        let room_for_commit = room_id.clone();
        let member_for_commit = member_id.clone();
        let run_for_commit = run_id.clone();
        tokio::task::spawn_blocking(move || {
            repository.request_interrupt_checked(
                &CollaborationActor::local(),
                &room_for_commit,
                &member_for_commit,
                &run_for_commit,
                expected_version,
            )
        })
        .await
        .map_err(|error| format!("中断成员运行失败: {error}"))?
        .map_err(|error| error.to_string())?;

        self.cancel_task_and_runtime(&room_id, &member_id, &run_id)
            .await;
        self.publish_snapshot(room_id).await;
        Ok(())
    }

    async fn change_availability(
        &self,
        room_id: String,
        member_id: String,
        command: AvailabilityCommand,
    ) -> Result<BrainMemberView, String> {
        let repository = Arc::clone(&self.repository);
        let room_for_commit = room_id.clone();
        let member = tokio::task::spawn_blocking(move || match command {
            AvailabilityCommand::Sleep => repository.sleep_member(&room_for_commit, &member_id),
            AvailabilityCommand::Archive => repository.archive_member(&room_for_commit, &member_id),
            AvailabilityCommand::Restore => repository.restore_member(&room_for_commit, &member_id),
        })
        .await
        .map_err(|error| format!("更新成员状态失败: {error}"))?
        .map_err(|error| error.to_string())?;
        self.member_changed(room_id, member.clone()).await;
        Ok(member)
    }

    async fn change_availability_checked(
        &self,
        room_id: String,
        member_id: String,
        expected_version: u64,
        command: AvailabilityCommand,
    ) -> Result<BrainMemberView, String> {
        let repository = Arc::clone(&self.repository);
        let room_for_commit = room_id.clone();
        let member = tokio::task::spawn_blocking(move || match command {
            AvailabilityCommand::Sleep => repository.sleep_member_checked(
                &CollaborationActor::local(),
                &room_for_commit,
                &member_id,
                expected_version,
            ),
            AvailabilityCommand::Archive => repository.archive_member_checked(
                &CollaborationActor::local(),
                &room_for_commit,
                &member_id,
                expected_version,
            ),
            AvailabilityCommand::Restore => repository.restore_member_checked(
                &CollaborationActor::local(),
                &room_for_commit,
                &member_id,
                expected_version,
            ),
        })
        .await
        .map_err(|error| format!("更新成员状态失败: {error}"))?
        .map_err(|error| error.to_string())?;
        self.member_changed(room_id, member.clone()).await;
        Ok(member)
    }

    async fn cancel_task_and_runtime(&self, room_id: &str, member_id: &str, run_id: &str) {
        let task_repository = Arc::clone(&self.task_repository);
        let task_run = run_id.to_string();
        match tokio::task::spawn_blocking(move || {
            let instance = task_repository.instance(&task_run)?;
            let task = task_repository.task(&instance.task_run_id)?;
            task_repository.cancel_task(&task.task_run_id, task.version)
        })
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(run_id, "持久 TaskRun 取消未收敛: {error}");
            }
            Err(error) => {
                tracing::warn!(run_id, "持久 TaskRun 取消线程异常: {error}");
            }
        }

        if let Some(active) = self.active_runs.lock().await.get(run_id).cloned() {
            if active.room_id == room_id && active.member_id == member_id {
                active.cancel.cancel();
            }
        }
    }

    async fn member_changed(&self, room_id: String, member: BrainMemberView) {
        self.broadcast(WebProgressEvent::MemberChanged { member });
        self.publish_snapshot(room_id).await;
        self.dispatcher_notify.notify_waiters();
    }

    async fn publish_snapshot(&self, room_id: String) {
        match self.snapshot(room_id).await {
            Ok(snapshot) => self.broadcast(WebProgressEvent::RoomSnapshot { snapshot }),
            Err(error) => tracing::warn!("广播协作房间快照失败: {error}"),
        }
    }

    fn broadcast(&self, event: WebProgressEvent) {
        let _ = self.events.send(event);
    }

    async fn publish_pending_outbox(&self) {
        let repository = Arc::clone(&self.repository);
        let pending = match tokio::task::spawn_blocking(move || {
            repository.pending_outbox_events(EVENT_CHANNEL_CAPACITY)
        })
        .await
        {
            Ok(Ok(events)) => events,
            Ok(Err(error)) => {
                tracing::warn!("读取协作 outbox 失败: {error}");
                return;
            }
            Err(error) => {
                tracing::warn!("读取协作 outbox 线程失败: {error}");
                return;
            }
        };
        let mut refreshed_rooms = HashSet::new();
        for event in pending {
            if !refreshed_rooms.contains(&event.room_id) {
                let Ok(snapshot) = self.snapshot(event.room_id.clone()).await else {
                    continue;
                };
                self.broadcast(WebProgressEvent::RoomSnapshot { snapshot });
                refreshed_rooms.insert(event.room_id.clone());
            }
            let repository = Arc::clone(&self.repository);
            let outbox_event_id = event.outbox_event_id.clone();
            match tokio::task::spawn_blocking(move || {
                repository.mark_outbox_published(&outbox_event_id, event.version)
            })
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!("发布协作 outbox 失败: {error}"),
                Err(error) => tracing::warn!("发布协作 outbox 线程失败: {error}"),
            }
        }
    }

    async fn dispatch_loop(self: Arc<Self>) {
        loop {
            self.publish_pending_outbox().await;
            let repository = Arc::clone(&self.repository);
            let claim = match tokio::task::spawn_blocking(move || repository.lease_next()).await {
                Ok(Ok(claim)) => claim,
                Ok(Err(error)) => {
                    tracing::error!("认领成员任务失败: {error}");
                    tokio::time::sleep(IDLE_POLL_INTERVAL).await;
                    continue;
                }
                Err(error) => {
                    tracing::error!("成员任务认领线程异常: {error}");
                    tokio::time::sleep(IDLE_POLL_INTERVAL).await;
                    continue;
                }
            };

            let Some(claim) = claim else {
                tokio::select! {
                    () = self.dispatcher_notify.notified() => {}
                    () = tokio::time::sleep(IDLE_POLL_INTERVAL) => {}
                }
                continue;
            };

            self.publish_snapshot(claim.room_id.clone()).await;
            let runtime = Arc::clone(&self);
            tokio::spawn(async move {
                runtime.schedule_claim(claim).await;
            });
        }
    }

    async fn schedule_claim(self: Arc<Self>, claim: ClaimedInboxItem) {
        let (task, context_snapshot, execution_policy) = match self.prepare_task(&claim).await {
            Ok(prepared) => prepared,
            Err(error) => {
                self.fail_leased_claim(&claim, error).await;
                return;
            }
        };
        if task.state == TaskRunState::Completed {
            self.reconcile_durable_claim(&claim).await;
            return;
        }
        let node_id = format!("node-{}", claim.inbox_item_id);
        let coordinated = match self
            .coordinator
            .admit_node(&node_id, &claim.run_id, CancellationToken::new())
            .await
        {
            Ok(coordinated) => coordinated,
            Err(error) => {
                self.fail_leased_claim(&claim, format!("持久任务统一准入失败: {error}"))
                    .await;
                return;
            }
        };
        let (started, admission) = coordinated.into_parts();

        let activation_repository = Arc::clone(&self.repository);
        let claim_for_activation = claim.clone();
        let active_claim = match tokio::task::spawn_blocking(move || {
            activation_repository.activate_lease(&claim_for_activation)
        })
        .await
        {
            Ok(Ok(active)) => active,
            Ok(Err(error)) => {
                tracing::warn!(run_id = %claim.run_id, "激活成员执行租约失败: {error}");
                self.abort_unactivated_node(&claim, &started, error.to_string())
                    .await;
                return;
            }
            Err(error) => {
                tracing::warn!(run_id = %claim.run_id, "激活成员执行租约线程失败: {error}");
                self.abort_unactivated_node(&claim, &started, error.to_string())
                    .await;
                return;
            }
        };
        self.execute_claim(
            active_claim,
            admission,
            started,
            context_snapshot,
            execution_policy,
        )
        .await;
    }

    async fn prepare_task(
        &self,
        claim: &ClaimedInboxItem,
    ) -> Result<(TaskRun, ContextSnapshot, MemberExecutionPolicy), String> {
        let task_run_id = format!("task-{}", claim.inbox_item_id);
        let tasks = Arc::clone(&self.task_repository);
        let task_id_for_lookup = task_run_id.clone();
        let existing = match tokio::task::spawn_blocking(move || tasks.task(&task_id_for_lookup))
            .await
            .map_err(|error| format!("读取持久任务线程失败: {error}"))?
        {
            Ok(task) => Some(task),
            Err(TaskEngineError::NotFound { .. }) => None,
            Err(error) => return Err(format!("读取持久任务失败: {error}")),
        };
        if let Some(task) = existing {
            validate_task_claim_identity(&task, claim)?;
            let snapshot = context_snapshot_from_task(&task)?;
            let policy = execution_policy_from_task(&task)?;
            return Ok((task, snapshot, policy));
        }

        let model = self
            .model_policy_details
            .iter()
            .find(|policy| policy.policy_id == claim.model_policy)
            .cloned()
            .ok_or_else(|| format!("成员模型策略未解析: {}", claim.model_policy))?;
        let history_repository = Arc::clone(&self.repository);
        let claim_for_context = claim.clone();
        let context_config = self.repository.config().clone();
        let context_builder = self.orchestrator.context_builder();
        let snapshot = tokio::task::spawn_blocking(move || {
            let history = history_repository
                .member_history(&claim_for_context)
                .map_err(|error| error.to_string())?;
            let request = context_request_for_claim(&claim_for_context, &history, &context_config)?;
            context_builder
                .build(&request)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| format!("构建成员上下文线程失败: {error}"))?
        .map_err(|error| format!("构建成员上下文失败: {error}"))?;

        let request = task_request_for_claim(claim, self.repository.config(), &model, &snapshot);
        let tasks = Arc::clone(&self.task_repository);
        let task = tokio::task::spawn_blocking(move || tasks.create_task(request))
            .await
            .map_err(|error| format!("创建持久任务线程失败: {error}"))?
            .map_err(|error| format!("创建持久任务失败: {error}"))?;
        let persisted_snapshot = context_snapshot_from_task(&task)?;
        let policy = execution_policy_from_task(&task)?;
        Ok((task, persisted_snapshot, policy))
    }

    async fn execute_claim(
        self: Arc<Self>,
        claim: ClaimedInboxItem,
        _admission: AdmissionLease,
        started: StartedNode,
        context_snapshot: ContextSnapshot,
        execution_policy: MemberExecutionPolicy,
    ) {
        if let Err(error) = self
            .run_claim(&claim, &started, context_snapshot, &execution_policy)
            .await
        {
            tracing::warn!(
                room_id = %claim.room_id,
                member_id = %claim.member_id,
                run_id = %claim.run_id,
                "成员运行结束: {error}"
            );
            self.fail_unsettled_claim(&claim, &started, error).await;
        }
        self.active_runs.lock().await.remove(&claim.run_id);
        self.publish_snapshot(claim.room_id.clone()).await;
        self.dispatcher_notify.notify_waiters();
    }

    #[allow(clippy::too_many_lines)]
    async fn run_claim(
        &self,
        claim: &ClaimedInboxItem,
        started: &StartedNode,
        context_snapshot: ContextSnapshot,
        execution_policy: &MemberExecutionPolicy,
    ) -> Result<(), ClaimRunError> {
        let memory_scope =
            ConversationMemoryScope::new(&claim.member_id, &claim.run_id).map_err(|error| {
                ClaimRunError::pre_execution(format!("创建成员记忆作用域失败: {error}"))
            })?;
        let participation = claim.purpose == InboxPurpose::Participation;
        let (mut progress, handle, cancel) = self.orchestrator.query_member_streaming_scoped(
            context_snapshot,
            memory_scope,
            &execution_policy.model_policy,
            &execution_policy.reasoning_depth,
            execution_policy.allow_tools,
        );
        self.active_runs.lock().await.insert(
            claim.run_id.clone(),
            ActiveRun {
                room_id: claim.room_id.clone(),
                member_id: claim.member_id.clone(),
                cancel: cancel.clone(),
            },
        );

        let interrupt_repository = Arc::clone(&self.repository);
        let claim_for_interrupt = claim.clone();
        let interrupt_requested = tokio::task::spawn_blocking(move || {
            interrupt_repository.interrupt_requested(&claim_for_interrupt)
        })
        .await
        .map_err(|error| ClaimRunError::after_execution(format!("检查中断状态失败: {error}")))?
        .map_err(|error| ClaimRunError::after_execution(error.to_string()))?;
        if interrupt_requested {
            cancel.cancel();
        }

        while let Some(event) = progress.recv().await {
            if participation {
                continue;
            }
            let Some(web_event) = WebProgressEvent::from_progress(&event) else {
                continue;
            };
            if matches!(web_event, WebProgressEvent::Done) {
                continue;
            }
            self.broadcast(WebProgressEvent::MemberRunProgress {
                room_id: claim.room_id.clone(),
                member_id: claim.member_id.clone(),
                run_id: claim.run_id.clone(),
                event: Box::new(web_event),
            });
        }

        let result = handle.await.map_err(|error| {
            ClaimRunError::after_execution(format!("成员运行任务异常结束: {error}"))
        })?;
        match result {
            Ok(output) => {
                let usage = ActualUsage {
                    input_tokens: output.usage.prompt_tokens,
                    output_tokens: output.usage.completion_tokens,
                };
                let interrupt_repository = Arc::clone(&self.repository);
                let claim_for_interrupt = claim.clone();
                let interrupted = cancel.is_cancelled()
                    || tokio::task::spawn_blocking(move || {
                        interrupt_repository.interrupt_requested(&claim_for_interrupt)
                    })
                    .await
                    .map_err(|error| {
                        ClaimRunError::with_runtime_usage(
                            format!("复核中断状态线程失败: {error}"),
                            usage,
                        )
                    })?
                    .map_err(|error| ClaimRunError::with_runtime_usage(error.to_string(), usage))?;
                if interrupted {
                    return Err(ClaimRunError::with_runtime_usage("运行已中断", usage));
                }

                let final_answer = output.answer;
                let artifact_repository = Arc::clone(&self.task_repository);
                let run_for_artifact = claim.run_id.clone();
                let answer_for_artifact = final_answer.clone();
                let artifact = tokio::task::spawn_blocking(move || {
                    artifact_repository.store_artifact(
                        &run_for_artifact,
                        &answer_for_artifact,
                        "text/plain; charset=utf-8",
                    )
                })
                .await
                .map_err(|error| {
                    ClaimRunError::with_runtime_usage(
                        format!("持久化任务产物线程失败: {error}"),
                        usage,
                    )
                })?
                .map_err(|error| {
                    ClaimRunError::with_runtime_usage(format!("持久化任务产物失败: {error}"), usage)
                })?;
                let task_repository = Arc::clone(&self.task_repository);
                let run_for_completion = claim.run_id.clone();
                let artifact_id = artifact.artifact_id.clone();
                let instance_version = started.instance.version;
                tokio::task::spawn_blocking(move || {
                    task_repository.complete_node(
                        &run_for_completion,
                        instance_version,
                        usage,
                        Some(&artifact_id),
                    )
                })
                .await
                .map_err(|error| {
                    ClaimRunError::with_runtime_usage(
                        format!("结算持久任务线程失败: {error}"),
                        usage,
                    )
                })?
                .map_err(|error| {
                    ClaimRunError::with_runtime_usage(format!("结算持久任务失败: {error}"), usage)
                })?;

                let completion_repository = Arc::clone(&self.repository);
                let claim_for_completion = claim.clone();
                let event = tokio::task::spawn_blocking(move || {
                    if claim_for_completion.purpose == InboxPurpose::Participation {
                        let answer = parse_participation_answer(&final_answer);
                        completion_repository
                            .complete_participation_item(&claim_for_completion, answer.as_deref())
                            .map(
                                |completion| match (completion.disposition, completion.event) {
                                    (ParticipationDisposition::Replied, Some(event)) => {
                                        MemberCompletion::Published(Box::new(event))
                                    }
                                    (ParticipationDisposition::Silent, _) => {
                                        MemberCompletion::Silent
                                    }
                                    (ParticipationDisposition::Suppressed, _)
                                    | (ParticipationDisposition::Replied, None) => {
                                        MemberCompletion::Suppressed
                                    }
                                    (ParticipationDisposition::Cancelled, _) => {
                                        MemberCompletion::Cancelled
                                    }
                                },
                            )
                    } else {
                        completion_repository
                            .complete_item(&claim_for_completion, &final_answer)
                            .map(|event| match event {
                                Some(event) => MemberCompletion::Published(Box::new(event)),
                                None => MemberCompletion::Cancelled,
                            })
                    }
                })
                .await
                .map_err(|error| {
                    ClaimRunError::with_runtime_usage(format!("提交成员结果失败: {error}"), usage)
                })?
                .map_err(|error| ClaimRunError::with_runtime_usage(error.to_string(), usage))?;
                match event {
                    MemberCompletion::Published(event) => {
                        self.broadcast(WebProgressEvent::MemberRunProgress {
                            room_id: claim.room_id.clone(),
                            member_id: claim.member_id.clone(),
                            run_id: claim.run_id.clone(),
                            event: Box::new(WebProgressEvent::FinalAnswer {
                                content: event.content.clone(),
                            }),
                        });
                        self.broadcast(WebProgressEvent::RoomEventAppended { event: *event });
                        self.finish_run(claim, "completed", None);
                    }
                    MemberCompletion::Silent | MemberCompletion::Suppressed => {
                        self.finish_run(claim, "completed", None);
                    }
                    MemberCompletion::Cancelled => self.finish_run(claim, "cancelled", None),
                }
            }
            Err(error) => {
                return Err(if error.execution_started() {
                    ClaimRunError::after_execution(error.to_string())
                } else {
                    ClaimRunError::pre_execution(error.to_string())
                });
            }
        }
        self.broadcast(WebProgressEvent::MemberRunProgress {
            room_id: claim.room_id.clone(),
            member_id: claim.member_id.clone(),
            run_id: claim.run_id.clone(),
            event: Box::new(WebProgressEvent::Done),
        });
        Ok(())
    }

    fn finish_run(&self, claim: &ClaimedInboxItem, status: &str, error: Option<String>) {
        self.broadcast(WebProgressEvent::MemberRunFinished {
            room_id: claim.room_id.clone(),
            member_id: claim.member_id.clone(),
            run_id: claim.run_id.clone(),
            status: status.into(),
            error,
        });
    }

    async fn abort_unactivated_node(
        &self,
        claim: &ClaimedInboxItem,
        started: &StartedNode,
        error: String,
    ) {
        let tasks = Arc::clone(&self.task_repository);
        let run_id = started.instance.instance_run_id.clone();
        let instance_version = started.instance.version;
        let durable_error = format!("协作租约激活失败: {error}");
        match tokio::task::spawn_blocking(move || {
            tasks.fail_node(&run_id, instance_version, &durable_error, true)
        })
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(task_error)) => tracing::error!("回收未激活持久节点失败: {task_error}"),
            Err(join_error) => tracing::error!("回收未激活持久节点线程异常: {join_error}"),
        }
        let repository = Arc::clone(&self.repository);
        let claim_for_release = claim.clone();
        match tokio::task::spawn_blocking(move || repository.release_lease(&claim_for_release))
            .await
        {
            Ok(Ok(())) => {}
            Ok(Err(release_error)) => tracing::error!("释放未激活成员租约失败: {release_error}"),
            Err(join_error) => tracing::error!("释放未激活成员租约线程异常: {join_error}"),
        }
        self.publish_snapshot(claim.room_id.clone()).await;
        self.dispatcher_notify.notify_waiters();
    }

    async fn fail_leased_claim(&self, claim: &ClaimedInboxItem, error: String) {
        let repository = Arc::clone(&self.repository);
        let claim_for_activation = claim.clone();
        match tokio::task::spawn_blocking(move || repository.activate_lease(&claim_for_activation))
            .await
        {
            Ok(Ok(active)) => self.fail_active_claim(&active, error).await,
            Ok(Err(activation_error)) => {
                tracing::error!(
                    run_id = %claim.run_id,
                    "激活失败任务的租约失败: {activation_error}; 原始错误: {error}"
                );
                let repository = Arc::clone(&self.repository);
                let claim_for_release = claim.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    repository.release_lease(&claim_for_release)
                })
                .await;
            }
            Err(join_error) => {
                tracing::error!(
                    run_id = %claim.run_id,
                    "激活失败任务的租约线程异常: {join_error}; 原始错误: {error}"
                );
            }
        }
        self.publish_snapshot(claim.room_id.clone()).await;
        self.dispatcher_notify.notify_waiters();
    }

    async fn fail_active_claim(&self, claim: &ClaimedInboxItem, error: String) {
        let repository = Arc::clone(&self.repository);
        let claim_for_failure = claim.clone();
        let error_for_commit = error.clone();
        match tokio::task::spawn_blocking(move || {
            repository.fail_item(&claim_for_failure, &error_for_commit)
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(commit_error)) => tracing::error!("提交成员失败状态失败: {commit_error}"),
            Err(join_error) => tracing::error!("提交成员失败状态线程异常: {join_error}"),
        }
        self.broadcast(WebProgressEvent::MemberRunProgress {
            room_id: claim.room_id.clone(),
            member_id: claim.member_id.clone(),
            run_id: claim.run_id.clone(),
            event: Box::new(WebProgressEvent::Error {
                message: error.clone(),
            }),
        });
        self.finish_run(claim, "failed", Some(error));
        self.broadcast(WebProgressEvent::MemberRunProgress {
            room_id: claim.room_id.clone(),
            member_id: claim.member_id.clone(),
            run_id: claim.run_id.clone(),
            event: Box::new(WebProgressEvent::Done),
        });
    }

    async fn reconcile_durable_claim(&self, claim: &ClaimedInboxItem) {
        let tasks = Arc::clone(&self.task_repository);
        let origin_id = claim.inbox_item_id.clone();
        let result = tokio::task::spawn_blocking(move || {
            tasks.completed_results("member_inbox").map(|results| {
                results
                    .into_iter()
                    .find(|result| result.origin_id == origin_id)
            })
        })
        .await;
        let durable = match result {
            Ok(Ok(Some(result))) => result,
            Ok(Ok(None)) => {
                self.fail_leased_claim(claim, "持久任务已完成但缺少可恢复产物".into())
                    .await;
                return;
            }
            Ok(Err(error)) => {
                self.fail_leased_claim(claim, format!("读取可恢复任务产物失败: {error}"))
                    .await;
                return;
            }
            Err(error) => {
                self.fail_leased_claim(claim, format!("读取可恢复任务产物线程失败: {error}"))
                    .await;
                return;
            }
        };
        let repository = Arc::clone(&self.repository);
        let claim_for_completion = claim.clone();
        let event = tokio::task::spawn_blocking(move || {
            let participation_answer =
                if claim_for_completion.purpose == InboxPurpose::Participation {
                    parse_participation_answer(&durable.artifact.content)
                } else {
                    Some(durable.artifact.content.clone())
                };
            repository.reconcile_claim_result(
                &claim_for_completion,
                &durable.instance_run_id,
                participation_answer.as_deref(),
            )
        })
        .await;
        match event {
            Ok(Ok(completion)) => {
                if let Some(event) = completion.event {
                    self.broadcast(WebProgressEvent::RoomEventAppended { event });
                }
            }
            Ok(Err(error)) => tracing::error!("补齐持久成员回复失败: {error}"),
            Err(error) => tracing::error!("补齐持久成员回复线程异常: {error}"),
        }
        self.publish_snapshot(claim.room_id.clone()).await;
    }

    #[allow(clippy::too_many_lines)]
    async fn fail_unsettled_claim(
        &self,
        claim: &ClaimedInboxItem,
        started: &StartedNode,
        failure: ClaimRunError,
    ) {
        let ClaimRunError {
            message: error,
            accounting,
        } = failure;
        let cancelled = self
            .active_runs
            .lock()
            .await
            .get(&claim.run_id)
            .is_some_and(|run| run.cancel.is_cancelled());
        let task_repository = Arc::clone(&self.task_repository);
        let run_for_failure = claim.run_id.clone();
        let durable_error = error.clone();
        let instance_version = started.instance.version;
        let durable_state = tokio::task::spawn_blocking(move || match accounting {
            FailedRunAccounting::PreExecution => task_repository.fail_node(
                &run_for_failure,
                instance_version,
                &durable_error,
                cancelled,
            ),
            FailedRunAccounting::RuntimeUsage(usage) => task_repository.fail_node_after_execution(
                &run_for_failure,
                instance_version,
                &durable_error,
                Some(usage),
                cancelled,
            ),
            FailedRunAccounting::ReservationUpperBound => task_repository
                .fail_node_after_execution(
                    &run_for_failure,
                    instance_version,
                    &durable_error,
                    None,
                    cancelled,
                ),
        })
        .await;
        match durable_state {
            Ok(Ok(instance)) if instance.state == InstanceRunState::Succeeded => {
                let Some(artifact_id) = instance.artifact_id else {
                    tracing::error!(run_id = %claim.run_id, "已完成任务缺少 artifact_id");
                    return;
                };
                let tasks = Arc::clone(&self.task_repository);
                let artifact =
                    tokio::task::spawn_blocking(move || tasks.artifact(&artifact_id)).await;
                match artifact {
                    Ok(Ok(artifact)) => {
                        let repository = Arc::clone(&self.repository);
                        let claim_for_completion = claim.clone();
                        let run_id = claim.run_id.clone();
                        match tokio::task::spawn_blocking(move || {
                            let answer =
                                if claim_for_completion.purpose == InboxPurpose::Participation {
                                    parse_participation_answer(&artifact.content)
                                } else {
                                    Some(artifact.content.clone())
                                };
                            repository.reconcile_claim_result(
                                &claim_for_completion,
                                &run_id,
                                answer.as_deref(),
                            )
                        })
                        .await
                        {
                            Ok(Ok(ParticipationCompletion {
                                event: Some(event), ..
                            })) => {
                                self.broadcast(WebProgressEvent::MemberRunProgress {
                                    room_id: claim.room_id.clone(),
                                    member_id: claim.member_id.clone(),
                                    run_id: claim.run_id.clone(),
                                    event: Box::new(WebProgressEvent::FinalAnswer {
                                        content: event.content.clone(),
                                    }),
                                });
                                self.broadcast(WebProgressEvent::RoomEventAppended { event });
                            }
                            Ok(Ok(_)) => {}
                            Ok(Err(commit_error)) => {
                                tracing::error!("补偿提交成员完成事件失败: {commit_error}");
                            }
                            Err(join_error) => {
                                tracing::error!("补偿提交成员完成事件线程异常: {join_error}");
                            }
                        }
                        self.finish_run(claim, "completed", None);
                        self.broadcast(WebProgressEvent::MemberRunProgress {
                            room_id: claim.room_id.clone(),
                            member_id: claim.member_id.clone(),
                            run_id: claim.run_id.clone(),
                            event: Box::new(WebProgressEvent::Done),
                        });
                        return;
                    }
                    Ok(Err(artifact_error)) => {
                        tracing::error!("读取已完成任务产物失败: {artifact_error}");
                    }
                    Err(join_error) => tracing::error!("读取已完成任务产物线程异常: {join_error}"),
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(task_error)) => tracing::error!("提交持久任务兜底终态失败: {task_error}"),
            Err(join_error) => tracing::error!("提交持久任务兜底终态线程异常: {join_error}"),
        }
        let repository = Arc::clone(&self.repository);
        let claim_for_failure = claim.clone();
        let error_for_commit = error.clone();
        match tokio::task::spawn_blocking(move || {
            repository.fail_item(&claim_for_failure, &error_for_commit)
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(commit_error)) => {
                tracing::error!("提交成员运行兜底终态失败: {commit_error}");
            }
            Err(join_error) => {
                tracing::error!("成员运行兜底终态线程异常: {join_error}");
            }
        }
        self.broadcast(WebProgressEvent::MemberRunProgress {
            room_id: claim.room_id.clone(),
            member_id: claim.member_id.clone(),
            run_id: claim.run_id.clone(),
            event: Box::new(WebProgressEvent::Error {
                message: if cancelled {
                    "运行已中断".into()
                } else {
                    error.clone()
                },
            }),
        });
        self.finish_run(
            claim,
            if cancelled { "cancelled" } else { "failed" },
            if cancelled { None } else { Some(error) },
        );
        self.broadcast(WebProgressEvent::MemberRunProgress {
            room_id: claim.room_id.clone(),
            member_id: claim.member_id.clone(),
            run_id: claim.run_id.clone(),
            event: Box::new(WebProgressEvent::Done),
        });
    }
}

#[derive(Clone, Copy)]
enum AvailabilityCommand {
    Sleep,
    Archive,
    Restore,
}

fn resolve_model_policy_details(
    policy_ids: &[String],
    llm_config: &LlmConfig,
) -> Vec<ResolvedModelPolicy> {
    policy_ids
        .iter()
        .map(|policy_id| llm_config.resolve_model_policy(policy_id))
        .collect()
}

#[cfg(test)]
fn scheduler_limits(config: &CollaborationConfig) -> task_engine::SchedulerLimits {
    config.scheduler_limits()
}

fn parse_participation_answer(answer: &str) -> Option<String> {
    let answer = answer.trim();
    if answer.eq_ignore_ascii_case("[[NO_REPLY]]") {
        None
    } else {
        Some(answer.to_owned())
    }
}

fn reconcile_durable_result(
    repository: &CollaborationRepository,
    inbox_item_id: &str,
    durable_run_id: &str,
    artifact_content: &str,
) -> std::result::Result<Option<ParticipationCompletion>, CollaborationError> {
    let Some(claim) = repository.claim_for_reconciliation(inbox_item_id, durable_run_id)? else {
        return Ok(None);
    };
    let answer = if claim.purpose == InboxPurpose::Participation {
        parse_participation_answer(artifact_content)
    } else {
        Some(artifact_content.to_owned())
    };
    repository
        .reconcile_claim_result(&claim, durable_run_id, answer.as_deref())
        .map(Some)
}

const MAX_CONTEXT_TOKENS: usize = 12_000;
const MAX_HISTORY_TOKENS: usize = 3_500;
const MAX_MEMORY_TOKENS: usize = 3_000;
const MAX_GRAPH_TOKENS: usize = 1_800;
const MAX_MEMORY_ITEMS: usize = 24;
const MAX_GRAPH_ITEMS: usize = 40;

fn validate_task_claim_identity(task: &TaskRun, claim: &ClaimedInboxItem) -> Result<(), String> {
    if task.origin_kind != "member_inbox"
        || task.origin_id != claim.inbox_item_id
        || task.room_id.as_deref() != Some(claim.room_id.as_str())
    {
        return Err(format!(
            "持久任务 {} 与成员收件箱 {} 身份不一致",
            task.task_run_id, claim.inbox_item_id
        ));
    }
    let source_event_id = task.resolved_config["source_event_id"]
        .as_str()
        .ok_or_else(|| format!("持久任务 {} 缺少 source_event_id", task.task_run_id))?;
    if source_event_id != claim.source_event_id {
        return Err(format!(
            "持久任务 {} 的来源事件与当前租约不一致",
            task.task_run_id
        ));
    }
    Ok(())
}

fn context_snapshot_from_task(task: &TaskRun) -> Result<ContextSnapshot, String> {
    if task.config_version != "collaboration-task-v3" {
        return Err(format!(
            "持久任务 {} 使用旧配置 {}，没有可重放的冻结上下文",
            task.task_run_id, task.config_version
        ));
    }
    let value = task
        .resolved_config
        .get("context_snapshot")
        .cloned()
        .ok_or_else(|| format!("持久任务 {} 缺少 context_snapshot", task.task_run_id))?;
    let snapshot: ContextSnapshot = serde_json::from_value(value)
        .map_err(|error| format!("解析持久上下文快照失败: {error}"))?;
    snapshot
        .validate()
        .map_err(|error| format!("校验持久上下文快照失败: {error}"))?;
    Ok(snapshot)
}

fn execution_policy_from_task(task: &TaskRun) -> Result<MemberExecutionPolicy, String> {
    let model_policy = task.resolved_config["model_policy"]["policy_id"]
        .as_str()
        .ok_or_else(|| format!("持久任务 {} 缺少模型策略", task.task_run_id))?;
    let reasoning_depth = task.resolved_config["reasoning_depth"]
        .as_str()
        .ok_or_else(|| format!("持久任务 {} 缺少思考深度", task.task_run_id))?;
    let purpose = task.resolved_config["purpose"]
        .as_str()
        .ok_or_else(|| format!("持久任务 {} 缺少执行目的", task.task_run_id))?;
    Ok(MemberExecutionPolicy {
        model_policy: model_policy.into(),
        reasoning_depth: reasoning_depth.into(),
        allow_tools: purpose != "participation",
    })
}

fn context_request_for_claim(
    claim: &ClaimedInboxItem,
    history: &[MemberHistoryMessage],
    config: &CollaborationConfig,
) -> Result<ContextRequest, String> {
    let tenant_id = TenantId::from("local");
    let namespace = NamespaceId::from("platform.core");
    let scopes = [
        ("room", claim.room_id.as_str()),
        ("member", claim.member_id.as_str()),
        ("instance_run", claim.run_id.as_str()),
    ]
    .into_iter()
    .map(|(scope_type, scope_key)| {
        ScopeRef::new(
            tenant_id.clone(),
            namespace.clone(),
            ScopeTypeId::from(scope_type),
            scope_key,
        )
        .map_err(|error| error.to_string())
    })
    .collect::<Result<Vec<_>, _>>()?;
    let total_tokens = usize::try_from(config.task_input_token_limit)
        .unwrap_or(usize::MAX)
        .min(MAX_CONTEXT_TOKENS);
    let budget = ContextBudget {
        max_total_tokens: total_tokens,
        max_optional_tokens: MAX_HISTORY_TOKENS.min(total_tokens.saturating_mul(3) / 10),
        max_memory_tokens: MAX_MEMORY_TOKENS.min(total_tokens / 4),
        max_graph_tokens: MAX_GRAPH_TOKENS.min(total_tokens.saturating_mul(15) / 100),
        max_items: 2usize
            .saturating_add(config.max_history_events_per_run)
            .saturating_add(MAX_MEMORY_ITEMS)
            .saturating_add(MAX_GRAPH_ITEMS),
    };
    let current_hash = sha256_hex(claim.input.as_bytes());
    let current_source = SourceRef::new(
        namespace.clone(),
        ResourceTypeId::from("conversation.turn"),
        claim.source_event_id.clone(),
        Some(claim.source_event_seq.to_string()),
        Some(current_hash.clone()),
    );
    let mut request = ContextRequest::new(tenant_id, scopes, vec![claim.input.clone()], budget)
        .with_required_block(ContextBlockInput::new(
            format!("member-policy:{}", claim.member_id),
            ContextBlockKind::SystemPolicy,
            member_policy_for_claim(claim),
        ))
        .with_required_block(
            ContextBlockInput::new(
                format!("current-input:{}", claim.inbox_item_id),
                ContextBlockKind::CurrentInput,
                execution_input_for_claim(claim),
            )
            .with_source_metadata(
                current_source,
                Some(claim.source_event_seq),
                Some(current_hash),
            ),
        );
    for message in history {
        if message.content.trim().is_empty() {
            continue;
        }
        let source = SourceRef::new(
            namespace.clone(),
            ResourceTypeId::from("conversation.turn"),
            message.event_id.clone(),
            Some(message.sequence.to_string()),
            Some(message.content_hash.clone()),
        );
        request = request.with_optional_block(
            ContextBlockInput::new(
                format!("room-event:{}", message.event_id),
                if message.role == "assistant" {
                    ContextBlockKind::ConversationAssistant
                } else {
                    ContextBlockKind::ConversationUser
                },
                message.content.clone(),
            )
            .with_source_metadata(
                source,
                Some(message.sequence),
                Some(message.content_hash.clone()),
            ),
        );
    }
    Ok(request)
}

fn member_policy_for_claim(claim: &ClaimedInboxItem) -> String {
    if claim.purpose == InboxPurpose::Participation {
        format!(
            "你是群聊中的独立智脑成员。\n成员名称：{}\n成员配置：{}\n\
             你收到的是共享群聊，不是直接任务。先判断自己是否能补充新的、相关且不重复的观点。\n\
             仅在以下情况发言：回答尚未回答的问题、补充关键证据、指出明确错误，或对其他智脑提出有理由的不同意见。\n\
             不要复述、附和、寒暄，不要调用工具或执行任何外部操作。\n\
             如果无需发言，只输出 [[NO_REPLY]]；如果需要发言，只输出要发送到群里的正文，不要解释你的选择。",
            claim.member_name, claim.profile_id,
        )
    } else {
        format!(
            "你是群聊中的独立智脑成员。\n成员名称：{}\n成员配置：{}\n当前模式：{}\n\
             只处理明确发给你的消息；不要冒充其他成员，也不要把其他成员未提供的内容当作自己的记忆。",
            claim.member_name,
            claim.profile_id,
            match claim.mode {
                RoomInputMode::Chat => "聊天",
                RoomInputMode::Task => "任务",
            }
        )
    }
}

fn execution_input_for_claim(claim: &ClaimedInboxItem) -> String {
    if claim.purpose == InboxPurpose::Participation {
        "请根据上面的群聊记录，决定现在是否需要发言。".into()
    } else {
        claim.input.clone()
    }
}

fn task_request_for_claim(
    claim: &ClaimedInboxItem,
    config: &CollaborationConfig,
    model: &ResolvedModelPolicy,
    context_snapshot: &ContextSnapshot,
) -> NewTaskRun {
    let task_run_id = format!("task-{}", claim.inbox_item_id);
    NewTaskRun {
        task_run_id: task_run_id.clone(),
        workflow: if claim.purpose == InboxPurpose::Participation {
            "collaboration.member-participation"
        } else {
            match claim.mode {
                RoomInputMode::Chat => "collaboration.member-chat",
                RoomInputMode::Task => "collaboration.member-task",
            }
        }
        .into(),
        objective: claim.input.clone(),
        origin_kind: "member_inbox".into(),
        origin_id: claim.inbox_item_id.clone(),
        room_id: Some(claim.room_id.clone()),
        config_version: "collaboration-task-v3".into(),
        resolved_config: serde_json::json!({
            "profile_id": claim.profile_id,
            "reasoning_depth": claim.reasoning_depth,
            "model_policy": model,
            "mode": claim.mode,
            "purpose": claim.purpose,
            "group_enabled": claim.group_enabled,
            "source_event_id": claim.source_event_id,
            "source_event_sequence": claim.source_event_seq,
            "conversation_root_event_id": claim.conversation_root_event_id,
            "context_through_sequence": claim.context_through_seq,
            "response_to_event_id": claim.response_to_event_id,
            "context_snapshot_id": context_snapshot.context_snapshot_id,
            "context_content_hash": context_snapshot.content_hash,
            "context_snapshot": context_snapshot,
            "budget": {
                "input_tokens": config.task_input_token_limit,
                "output_tokens": config.task_output_token_limit
            }
        }),
        parent_budget_account_id: None,
        budget: BudgetLimits {
            input_tokens: config.task_input_token_limit,
            output_tokens: config.task_output_token_limit,
        },
        nodes: vec![NewTaskNode {
            node_id: format!("node-{}", claim.inbox_item_id),
            kind: NodeKind::Model,
            dependencies: Vec::new(),
            provider: model.provider.clone(),
            model: model.model.clone(),
            profile: claim.profile_id.clone(),
            room_id: Some(claim.room_id.clone()),
            member_id: Some(claim.member_id.clone()),
            reservation: BudgetRequest {
                input_tokens: config.task_input_token_limit,
                output_tokens: config.task_output_token_limit,
            },
            retryable: true,
            side_effecting: false,
        }],
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        context_request_for_claim, context_snapshot_from_task, parse_participation_answer,
        reconcile_durable_result, resolve_model_policy_details, scheduler_limits,
        task_request_for_claim,
    };
    use crate::web::collaboration::{
        CollaborationConfig, CollaborationRepository, InboxPurpose, InboxState,
        MemberHistoryMessage, ParticipationDisposition, RoomInputMode,
    };
    use brain_llm::config::LlmConfig;
    use knowledge_core::{
        ContentResolverRegistry, ContextBlock, ContextBlockInput, ContextBlockKind, ContextBuilder,
        ContextSnapshot, GraphQueryPort, GraphQueryRequest, GraphQueryResult, KnowledgeError,
        MemoryQuery, MemoryQueryPort, MemoryQueryResult,
    };
    use task_engine::{
        ActualUsage, Scheduler, TaskCoordinator, TaskEngineError, TaskRepository, TaskRunState,
    };
    use tokio_util::sync::CancellationToken;

    fn task_context_snapshot(id: &str, input: &str) -> ContextSnapshot {
        ContextSnapshot::new(
            id,
            vec![
                ContextBlock::from_input(ContextBlockInput::new(
                    "member-policy",
                    ContextBlockKind::SystemPolicy,
                    "member policy",
                ))
                .unwrap(),
                ContextBlock::from_input(ContextBlockInput::new(
                    "current-input",
                    ContextBlockKind::CurrentInput,
                    input,
                ))
                .unwrap(),
            ],
        )
        .unwrap()
    }

    struct EmptyMemory;

    impl MemoryQueryPort for EmptyMemory {
        fn query(&self, _query: &MemoryQuery) -> Result<MemoryQueryResult, KnowledgeError> {
            Ok(MemoryQueryResult {
                entries: Vec::new(),
                scanned: 0,
                truncated: false,
            })
        }
    }

    struct UnavailableGraph;

    impl GraphQueryPort for UnavailableGraph {
        fn query(&self, _query: &GraphQueryRequest) -> Result<GraphQueryResult, KnowledgeError> {
            Err(KnowledgeError::Unavailable("test graph outage".into()))
        }
    }

    #[test]
    fn main_route_alias_resolves_to_concrete_web_model_metadata() {
        let config = LlmConfig::default_config();
        let details = resolve_model_policy_details(&[String::from("main")], &config);

        assert_eq!(details.len(), 1);
        assert_eq!(details[0].policy_id, "main");
        assert_eq!(details[0].provider, config.provider_for_brain("main"));
        assert_eq!(details[0].model, config.model_for_brain("main"));
        assert_ne!(details[0].model, details[0].policy_id);
    }

    #[test]
    fn participation_answer_distinguishes_silence_from_a_visible_opinion() {
        assert!(parse_participation_answer(" \n[[NO_REPLY]]\t").is_none());
        assert_eq!(
            parse_participation_answer("  我不同意，回滚路径还没有验证。  ").as_deref(),
            Some("我不同意，回滚路径还没有验证。")
        );
    }

    #[test]
    fn collaboration_limits_map_to_one_composite_scheduler_policy() {
        let config = CollaborationConfig::default();
        let limits = scheduler_limits(&config);

        assert_eq!(limits.max_workers, config.max_workers);
        assert_eq!(limits.max_global, config.max_global_runs);
        assert_eq!(limits.max_per_room, config.max_runs_per_room);
        assert_eq!(limits.max_per_member, 1);
        assert_eq!(limits.max_per_provider, config.max_runs_per_provider);
        assert_eq!(limits.max_per_profile, config.max_runs_per_profile);
        assert_eq!(limits.max_per_task, config.max_runs_per_task);
    }

    #[test]
    fn participation_task_snapshot_carries_group_lineage_and_purpose() {
        let directory = tempfile::tempdir().unwrap();
        let config = CollaborationConfig::default();
        let collaboration = CollaborationRepository::new(directory.path(), config.clone()).unwrap();
        let room = collaboration
            .ensure_room("room-1", "Task Room", &[])
            .unwrap();
        collaboration
            .create_member("room-1", "智脑 B", None, None)
            .unwrap();
        collaboration
            .post_group_message(
                "room-1",
                std::slice::from_ref(&room.room.default_member_id),
                "一起判断",
                RoomInputMode::Chat,
                "participation-task",
            )
            .unwrap();
        let _direct = collaboration.lease_next().unwrap().unwrap();
        let participation = collaboration.lease_next().unwrap().unwrap();
        assert_eq!(participation.purpose, InboxPurpose::Participation);

        let llm = LlmConfig::default_config();
        let model = llm.resolve_model_policy(&participation.model_policy);
        let context = task_context_snapshot("context-participation", "decide whether to reply");
        let request = task_request_for_claim(&participation, &config, &model, &context);

        assert_eq!(request.workflow, "collaboration.member-participation");
        assert_eq!(request.config_version, "collaboration-task-v3");
        assert_eq!(request.resolved_config["purpose"], "participation");
        assert_eq!(
            request.resolved_config["context_snapshot_id"],
            context.context_snapshot_id
        );
        assert_eq!(
            request.resolved_config["context_content_hash"],
            context.content_hash
        );
        assert_eq!(
            request.resolved_config["context_snapshot"]["blocks"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            request.resolved_config["conversation_root_event_id"],
            participation.conversation_root_event_id
        );
        assert_eq!(
            request.resolved_config["context_through_sequence"],
            participation.context_through_seq
        );
    }

    #[test]
    fn durable_silent_participation_reconciles_without_a_room_reply() {
        let directory = tempfile::tempdir().unwrap();
        let collaboration =
            CollaborationRepository::new(directory.path(), CollaborationConfig::default()).unwrap();
        let room = collaboration
            .ensure_room("room-1", "Task Room", &[])
            .unwrap();
        collaboration
            .create_member("room-1", "智脑 B", None, None)
            .unwrap();
        collaboration
            .post_group_message(
                "room-1",
                std::slice::from_ref(&room.room.default_member_id),
                "是否需要补充",
                RoomInputMode::Chat,
                "durable-silent",
            )
            .unwrap();
        let _direct = collaboration.lease_next().unwrap().unwrap();
        let participation = collaboration.lease_next().unwrap().unwrap();

        let completion = reconcile_durable_result(
            &collaboration,
            &participation.inbox_item_id,
            "durable-participation-run",
            "[[NO_REPLY]]",
        )
        .unwrap()
        .unwrap();

        assert_eq!(completion.disposition, ParticipationDisposition::Silent);
        assert!(completion.event.is_none());
        let persisted = collaboration.snapshot("room-1").unwrap();
        assert!(persisted
            .events
            .iter()
            .all(|event| event.kind != "member_message"));
    }

    #[test]
    fn restart_reuses_persisted_context_and_rejects_a_new_config_hash() {
        let directory = tempfile::tempdir().unwrap();
        let config = CollaborationConfig::default();
        let collaboration = CollaborationRepository::new(directory.path(), config.clone()).unwrap();
        let room = collaboration
            .ensure_room("room-1", "Task Room", &[])
            .unwrap();
        collaboration
            .post_message(
                "room-1",
                std::slice::from_ref(&room.room.default_member_id),
                "冻结这个输入",
                RoomInputMode::Task,
                "frozen-context",
            )
            .unwrap();
        let lease = collaboration.lease_next().unwrap().unwrap();
        let llm = LlmConfig::default_config();
        let model = llm.resolve_model_policy(&lease.model_policy);
        let original = task_context_snapshot("context-original", &lease.input);
        let tasks = TaskRepository::open(collaboration.database_path()).unwrap();
        let created = tasks
            .create_task(task_request_for_claim(&lease, &config, &model, &original))
            .unwrap();

        let persisted = context_snapshot_from_task(&created).unwrap();
        assert_eq!(persisted, original);
        let replayed = tasks
            .create_task(task_request_for_claim(&lease, &config, &model, &persisted))
            .unwrap();
        assert_eq!(replayed.config_content_hash, created.config_content_hash);

        let newer = task_context_snapshot("context-newer", "newer memory changed the input");
        assert!(matches!(
            tasks.create_task(task_request_for_claim(&lease, &config, &model, &newer)),
            Err(TaskEngineError::Invalid(_))
        ));

        let mut tampered = created;
        tampered.resolved_config["context_snapshot"]["blocks"][0]["content"] =
            serde_json::json!("tampered policy");
        assert!(context_snapshot_from_task(&tampered).is_err());
    }

    #[test]
    fn collaboration_context_is_budgeted_traceable_and_graph_degradable() {
        let directory = tempfile::tempdir().unwrap();
        let config = CollaborationConfig {
            task_input_token_limit: 1_000,
            max_history_events_per_run: 8,
            ..CollaborationConfig::default()
        };
        let collaboration = CollaborationRepository::new(directory.path(), config.clone()).unwrap();
        let room = collaboration
            .ensure_room("room-1", "Context Room", &[])
            .unwrap();
        collaboration
            .post_message(
                "room-1",
                std::slice::from_ref(&room.room.default_member_id),
                "第一条问题",
                RoomInputMode::Chat,
                "context-first",
            )
            .unwrap();
        let first = collaboration.claim_next().unwrap().unwrap();
        collaboration.complete_item(&first, "第一条回答").unwrap();
        collaboration
            .post_message(
                "room-1",
                std::slice::from_ref(&room.room.default_member_id),
                "第二条问题",
                RoomInputMode::Chat,
                "context-second",
            )
            .unwrap();
        let second = collaboration.claim_next().unwrap().unwrap();
        let mut history = collaboration.member_history(&second).unwrap();
        assert_eq!(history.len(), 2);
        history.insert(
            0,
            MemberHistoryMessage {
                event_id: "historical-blank-event".into(),
                sequence: 1,
                role: "assistant".into(),
                content: String::new(),
                content_hash: knowledge_core::sha256_hex(b""),
            },
        );

        let request = context_request_for_claim(&second, &history, &config).unwrap();
        let snapshot = ContextBuilder::new(
            Arc::new(EmptyMemory),
            Arc::new(UnavailableGraph),
            Arc::new(ContentResolverRegistry::new()),
        )
        .build(&request)
        .unwrap();

        assert!(snapshot.used_tokens <= snapshot.budget.max_total_tokens);
        assert_eq!(snapshot.budget.max_total_tokens, 1_000);
        assert!(snapshot
            .degradations
            .iter()
            .any(|item| item.source == "graph" && item.incomplete));
        assert!(snapshot
            .blocks
            .iter()
            .all(|block| block.block_id != "room-event:historical-blank-event"));
        for message in history
            .into_iter()
            .filter(|message| !message.content.trim().is_empty())
        {
            let block = snapshot
                .blocks
                .iter()
                .find(|block| block.block_id == format!("room-event:{}", message.event_id))
                .unwrap();
            assert_eq!(block.source_revision, Some(message.sequence));
            assert_eq!(
                block.source_hash.as_deref(),
                Some(message.content_hash.as_str())
            );
            assert_eq!(
                block.source_ref.as_ref().unwrap().resource_id,
                message.event_id
            );
        }
        let current = snapshot
            .blocks
            .iter()
            .find(|block| block.kind == ContextBlockKind::CurrentInput)
            .unwrap();
        assert_eq!(current.source_revision, Some(second.source_event_seq));
        assert_eq!(
            current.source_ref.as_ref().unwrap().resource_id,
            second.source_event_id
        );
    }

    #[tokio::test]
    async fn member_task_vertical_slice_is_durable_in_shared_runtime_database() {
        let directory = tempfile::tempdir().unwrap();
        let config = CollaborationConfig {
            max_workers: 1,
            max_global_runs: 1,
            max_runs_per_room: 1,
            max_runs_per_member: 1,
            max_runs_per_provider: 1,
            max_runs_per_profile: 1,
            max_runs_per_task: 1,
            task_input_token_limit: 1_000,
            task_output_token_limit: 500,
            ..CollaborationConfig::default()
        };
        let collaboration = CollaborationRepository::new(directory.path(), config.clone()).unwrap();
        let room = collaboration
            .ensure_room("room-1", "Task Room", &[])
            .unwrap();
        collaboration
            .post_message(
                "room-1",
                std::slice::from_ref(&room.room.default_member_id),
                "执行持久任务",
                RoomInputMode::Task,
                "task-command",
            )
            .unwrap();
        let lease = collaboration.lease_next().unwrap().unwrap();
        assert_eq!(
            collaboration.snapshot("room-1").unwrap().inbox[0].state,
            InboxState::Leased
        );

        let llm = LlmConfig::default_config();
        let model = llm.resolve_model_policy(&lease.model_policy);
        let context = task_context_snapshot("context-durable", &lease.input);
        let request = task_request_for_claim(&lease, &config, &model, &context);
        let node_id = request.nodes[0].node_id.clone();
        let tasks = Arc::new(TaskRepository::open(collaboration.database_path()).unwrap());
        let task = tasks.create_task(request).unwrap();
        let coordinator = TaskCoordinator::new(
            Arc::clone(&tasks),
            Scheduler::new(scheduler_limits(&config)).unwrap(),
        );
        let coordinated = coordinator
            .admit_node(&node_id, &lease.run_id, CancellationToken::new())
            .await
            .unwrap();
        let active = collaboration.activate_lease(&lease).unwrap();
        let artifact = tasks
            .store_artifact(
                &active.run_id,
                "持久任务已完成",
                "text/plain; charset=utf-8",
            )
            .unwrap();
        tasks
            .complete_node(
                &active.run_id,
                coordinated.started().instance.version,
                ActualUsage {
                    input_tokens: 123,
                    output_tokens: 45,
                },
                Some(&artifact.artifact_id),
            )
            .unwrap();
        drop(coordinated);

        let event = collaboration
            .reconcile_completed_item(&active.inbox_item_id, &active.run_id, &artifact.content)
            .unwrap()
            .unwrap();
        assert_eq!(event.content, "持久任务已完成");
        assert_eq!(
            collaboration.snapshot("room-1").unwrap().inbox[0].state,
            InboxState::Completed
        );
        assert_eq!(
            tasks.task(&task.task_run_id).unwrap().state,
            TaskRunState::Completed
        );
        let account = tasks.budget_account(&task.budget_account_id).unwrap();
        assert_eq!(account.reserved_input_tokens, 0);
        assert_eq!(account.consumed_input_tokens, 123);

        drop(tasks);
        let reopened = TaskRepository::open(collaboration.database_path()).unwrap();
        assert_eq!(reopened.recover_inflight().unwrap().interrupted, 0);
        let results = reopened.completed_results("member_inbox").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].artifact.content, "持久任务已完成");
    }
}
