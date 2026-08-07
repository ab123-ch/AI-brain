use std::sync::{Arc, Mutex};

use agent_runtime::{
    AgentProfileSnapshot, AgentRunSpec, AgentRunStatus, AgentRuntime, AgentRuntimeError,
    AgentWorkerPool, ArtifactEnvelope, ArtifactSink, BudgetReservation, ContextSnapshot,
    OutputContract, ReasoningPolicy, ResolvedModelPolicy, ToolGrant,
};
use runtime::{
    ApiClient, ApiRequest, AssistantEvent, ContentBlock, PermissionMode, PermissionPolicy,
    RuntimeError, TokenUsage, ToolError, ToolExecutor,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
struct RecordingSink {
    artifacts: Arc<Mutex<Vec<ArtifactEnvelope>>>,
}

impl ArtifactSink for RecordingSink {
    fn store(&self, artifact: &ArtifactEnvelope) -> Result<(), AgentRuntimeError> {
        self.artifacts.lock().unwrap().push(artifact.clone());
        Ok(())
    }
}

struct RecordingClient {
    requests: Arc<Mutex<Vec<ApiRequest>>>,
}

impl ApiClient for RecordingClient {
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        self.requests.lock().unwrap().push(request);
        Ok(vec![
            AssistantEvent::TextDelta("verified result".into()),
            AssistantEvent::Usage(TokenUsage {
                input_tokens: 11,
                output_tokens: 7,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 3,
            }),
            AssistantEvent::MessageStop,
        ])
    }
}

struct CancellingClient {
    cancellation: CancellationToken,
}

impl ApiClient for CancellingClient {
    fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        self.cancellation.cancel();
        Ok(vec![
            AssistantEvent::TextDelta("must not be committed".into()),
            AssistantEvent::Usage(TokenUsage {
                input_tokens: 5,
                output_tokens: 4,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            }),
            AssistantEvent::MessageStop,
        ])
    }
}

#[derive(Default)]
struct NoopToolExecutor;

impl ToolExecutor for NoopToolExecutor {
    fn execute(&mut self, tool_name: &str, _input: &str) -> Result<String, ToolError> {
        Err(ToolError::new(format!("unexpected tool: {tool_name}")))
    }
}

fn run_spec(role: &str) -> AgentRunSpec {
    AgentRunSpec {
        agent_instance_id: "agent-1".into(),
        instance_run_id: "run-1".into(),
        member_id: None,
        inbox_item_id: None,
        task_run_id: "task-1".into(),
        node_id: "node-1".into(),
        profile: AgentProfileSnapshot::new(
            format!("builtin.{}", role.to_ascii_lowercase()),
            1,
            role,
            vec![format!("profile prompt for {role}")],
            ToolGrant::new(["read_file"]),
            OutputContract::Text,
            8,
        )
        .unwrap(),
        context_snapshot: ContextSnapshot::from_text("context-1", "inspect the delegated input")
            .unwrap(),
        input_artifacts: Vec::new(),
        model: ResolvedModelPolicy {
            policy_id: "subagent".into(),
            label: "subagent".into(),
            provider: "mock".into(),
            model: "mock-model".into(),
            max_output_tokens: 1_024,
            temperature: 0.0,
        },
        reasoning: ReasoningPolicy::medium(),
        budget_reservation: BudgetReservation {
            reservation_id: "budget-1".into(),
            max_input_tokens: 10_000,
            max_output_tokens: 1_024,
        },
        deadline_unix_ms: None,
    }
}

#[tokio::test]
async fn profile_context_artifact_and_usage_are_traceable() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let sink = RecordingSink::default();
    let pool = AgentWorkerPool::new(1).unwrap();
    let cancellation = CancellationToken::new();
    let lease = pool.acquire(&cancellation).await.unwrap();
    let agent = AgentRuntime::new(
        RecordingClient {
            requests: Arc::clone(&requests),
        },
        NoopToolExecutor,
        PermissionPolicy::new(PermissionMode::DangerFullAccess),
        sink.clone(),
    );

    let outcome = agent
        .spawn(run_spec("Explore"), lease, cancellation)
        .wait()
        .await;

    assert_eq!(outcome.status, AgentRunStatus::Completed);
    assert_eq!(outcome.usage.input_tokens, 11);
    assert_eq!(outcome.usage.output_tokens, 7);
    let artifact = outcome.artifact.expect("completed run artifact");
    assert_eq!(artifact.content, "verified result");
    assert_eq!(artifact.profile_id, "builtin.explore");
    assert_eq!(artifact.context_snapshot_id, "context-1");
    assert_eq!(artifact.model.model, "mock-model");
    assert!(!artifact.content_hash.is_empty());
    assert_eq!(sink.artifacts.lock().unwrap().as_slice(), &[artifact]);

    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].system_prompt,
        vec!["profile prompt for Explore"]
    );
    assert!(requests[0].messages[0].blocks.iter().any(|block| matches!(
        block,
        ContentBlock::Text { text } if text.contains("inspect the delegated input")
    )));
    assert_eq!(pool.available_permits(), 1);
}

#[tokio::test]
async fn cancellation_after_model_return_prevents_artifact_commit() {
    let cancellation = CancellationToken::new();
    let sink = RecordingSink::default();
    let pool = AgentWorkerPool::new(1).unwrap();
    let lease = pool.acquire(&cancellation).await.unwrap();
    let agent = AgentRuntime::new(
        CancellingClient {
            cancellation: cancellation.clone(),
        },
        NoopToolExecutor,
        PermissionPolicy::new(PermissionMode::DangerFullAccess),
        sink.clone(),
    );

    let outcome = agent
        .spawn(run_spec("Verification"), lease, cancellation)
        .wait()
        .await;

    assert_eq!(outcome.status, AgentRunStatus::Cancelled);
    assert!(outcome.artifact.is_none());
    assert!(sink.artifacts.lock().unwrap().is_empty());
    assert_eq!(pool.available_permits(), 1);
}

#[tokio::test]
async fn cancelled_wait_does_not_consume_a_worker_lease() {
    let pool = AgentWorkerPool::new(1).unwrap();
    let held = pool.acquire(&CancellationToken::new()).await.unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let result = pool.acquire(&cancellation).await;

    assert!(matches!(result, Err(AgentRuntimeError::Cancelled)));
    assert_eq!(pool.available_permits(), 0);
    drop(held);
    assert_eq!(pool.available_permits(), 1);
}
