//! 仅在群消息 Claim 执行期间暴露的只读消息池工具。
//!
//! 工具的房间与可见序号均由服务端 Claim 固定，模型提供的参数不能扩大读取范围。

use std::sync::Arc;
use std::time::Instant;

use brain_core::tool_executor::{ToolDescriptor, ToolExecutionContext, ToolExecutor};
use brain_core::types::{ToolCall, ToolExecutionResult};
use brain_llm::ToolDefinition;
use serde::Serialize;

use crate::web::collaboration::{CollaborationRepository, RoomEventView};

const READ_GROUP_MESSAGES_TOOL: &str = "read_group_messages";
const DEFAULT_MESSAGE_LIMIT: usize = 20;
const MAX_MESSAGE_LIMIT: usize = 50;

/// 一次成员执行被允许读取的公共消息池范围。
#[derive(Clone)]
pub(crate) struct GroupMessageToolScope {
    repository: Arc<CollaborationRepository>,
    room_id: String,
    context_through_seq: u64,
}

impl GroupMessageToolScope {
    pub(crate) fn new(
        repository: Arc<CollaborationRepository>,
        room_id: impl Into<String>,
        context_through_seq: u64,
    ) -> Self {
        Self {
            repository,
            room_id: room_id.into(),
            context_through_seq,
        }
    }
}

/// 为当前群消息 Claim 添加只读消息池工具，同时保留原有工具执行能力。
pub(crate) struct GroupMessageToolExecutor {
    inner: Arc<dyn ToolExecutor>,
    scope: GroupMessageToolScope,
}

impl GroupMessageToolExecutor {
    pub(crate) fn new(inner: Arc<dyn ToolExecutor>, scope: GroupMessageToolScope) -> Self {
        Self { inner, scope }
    }
}

pub(crate) fn group_message_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: READ_GROUP_MESSAGES_TOOL.into(),
        description: "查看当前群聊在本次任务开始前的公共消息池。只能读取当前群，且不会返回本次任务上下文边界之后的消息。".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_MESSAGE_LIMIT,
                    "description": "返回消息数，默认 20。"
                },
                "before_sequence": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "可选分页游标；只返回该序号之前的消息。"
                }
            },
            "additionalProperties": false
        }),
    }
}

impl ToolExecutor for GroupMessageToolExecutor {
    fn execute(
        &self,
        tool_call: &ToolCall,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolExecutionResult> + Send + '_>> {
        if tool_call.tool_name != READ_GROUP_MESSAGES_TOOL {
            return self.inner.execute(tool_call);
        }

        let tool_name = tool_call.tool_name.clone();
        let room_id = self.scope.room_id.clone();
        let repository = Arc::clone(&self.scope.repository);
        let maximum_sequence = self.scope.context_through_seq;
        let requested_limit = tool_call
            .input
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(DEFAULT_MESSAGE_LIMIT)
            .clamp(1, MAX_MESSAGE_LIMIT);
        let through_sequence = tool_call
            .input
            .get("before_sequence")
            .and_then(serde_json::Value::as_u64)
            .map(|value| maximum_sequence.min(value.saturating_sub(1)))
            .unwrap_or(maximum_sequence);

        Box::pin(async move {
            let started = Instant::now();
            let result = repository
                .events_through(&room_id, through_sequence, requested_limit)
                .map_err(|error| error.to_string())
                .and_then(|events| {
                    serde_json::to_string_pretty(&GroupMessageToolOutput {
                        room_id,
                        context_through_sequence: maximum_sequence,
                        messages: events.iter().map(GroupMessageToolEvent::from).collect(),
                    })
                    .map_err(|error| error.to_string())
                });
            match result {
                Ok(output) => ToolExecutionResult {
                    tool_name,
                    output,
                    is_error: false,
                    duration_ms: started.elapsed().as_millis() as u64,
                },
                Err(error) => ToolExecutionResult {
                    tool_name,
                    output: format!("读取群消息失败: {error}"),
                    is_error: true,
                    duration_ms: started.elapsed().as_millis() as u64,
                },
            }
        })
    }

    fn execute_with_context<'a>(
        &'a self,
        tool_call: &'a ToolCall,
        context: &'a ToolExecutionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolExecutionResult> + Send + 'a>> {
        if tool_call.tool_name == READ_GROUP_MESSAGES_TOOL {
            return self.execute(tool_call);
        }
        self.inner.execute_with_context(tool_call, context)
    }

    fn list_tools(&self) -> Vec<ToolDescriptor> {
        let mut tools = self.inner.list_tools();
        if !tools
            .iter()
            .any(|tool| tool.name == READ_GROUP_MESSAGES_TOOL)
        {
            let definition = group_message_tool_definition();
            tools.push(ToolDescriptor {
                name: definition.name,
                description: definition.description,
                input_schema: definition.input_schema,
            });
        }
        tools
    }
}

#[derive(Serialize)]
struct GroupMessageToolOutput {
    room_id: String,
    context_through_sequence: u64,
    messages: Vec<GroupMessageToolEvent>,
}

#[derive(Serialize)]
struct GroupMessageToolEvent {
    sequence: u64,
    sender_kind: String,
    sender_name: String,
    kind: String,
    content: String,
}

impl From<&RoomEventView> for GroupMessageToolEvent {
    fn from(event: &RoomEventView) -> Self {
        Self {
            sequence: event.sequence,
            sender_kind: event.sender_kind.clone(),
            sender_name: event.sender_name.clone(),
            kind: event.kind.clone(),
            content: event.content.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use brain_core::tool_executor::{ToolExecutionContext, ToolExecutor};
    use brain_core::types::{ToolCall, ToolDescriptor, ToolExecutionResult};

    use super::{GroupMessageToolExecutor, GroupMessageToolScope, READ_GROUP_MESSAGES_TOOL};
    use crate::web::collaboration::{CollaborationRepository, RoomInputMode};

    #[derive(Default)]
    struct RecordingToolExecutor {
        contexts: Mutex<Vec<PathBuf>>,
        execute_calls: AtomicUsize,
    }

    impl ToolExecutor for RecordingToolExecutor {
        fn execute(
            &self,
            tool_call: &ToolCall,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolExecutionResult> + Send + '_>>
        {
            self.execute_calls.fetch_add(1, Ordering::SeqCst);
            let tool_name = tool_call.tool_name.clone();
            Box::pin(async move {
                ToolExecutionResult {
                    tool_name,
                    output: "inner-ok".into(),
                    is_error: false,
                    duration_ms: 0,
                }
            })
        }

        fn execute_with_context<'a>(
            &'a self,
            tool_call: &'a ToolCall,
            context: &'a ToolExecutionContext,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolExecutionResult> + Send + 'a>>
        {
            self.contexts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(context.working_directory.clone());
            self.execute(tool_call)
        }

        fn list_tools(&self) -> Vec<ToolDescriptor> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn explicit_working_directory_is_forwarded_by_group_wrapper() {
        let directory = tempfile::tempdir().unwrap();
        let working_directory = directory.path().canonicalize().unwrap();
        let repository =
            Arc::new(CollaborationRepository::new(&working_directory, Default::default()).unwrap());
        let inner = Arc::new(RecordingToolExecutor::default());
        let executor = GroupMessageToolExecutor::new(
            inner.clone(),
            GroupMessageToolScope::new(repository, "room-1", 0),
        );
        let call = ToolCall {
            tool_name: "read_file".into(),
            input: serde_json::json!({ "path": "same.txt" }),
            validated: true,
            validation_id: None,
        };

        let result = executor
            .execute_with_context(&call, &ToolExecutionContext::new(&working_directory))
            .await;

        assert!(!result.is_error);
        assert_eq!(
            *inner
                .contexts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![working_directory]
        );
    }

    #[tokio::test]
    async fn scoped_tool_cannot_read_messages_after_claim_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let repository =
            Arc::new(CollaborationRepository::new(directory.path(), Default::default()).unwrap());
        let room = repository.ensure_room("room-1", "Tool Room", &[]).unwrap();
        let first = repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&room.room.default_member_id),
                "边界内消息",
                RoomInputMode::Chat,
                "scoped-tool-1",
            )
            .unwrap();
        repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&room.room.default_member_id),
                "边界外消息",
                RoomInputMode::Chat,
                "scoped-tool-2",
            )
            .unwrap();
        let inner = Arc::new(RecordingToolExecutor::default());
        let executor = GroupMessageToolExecutor::new(
            inner.clone(),
            GroupMessageToolScope::new(Arc::clone(&repository), "room-1", first.event.sequence),
        );

        let result = executor
            .execute_with_context(
                &ToolCall {
                    tool_name: READ_GROUP_MESSAGES_TOOL.into(),
                    input: serde_json::json!({"limit": 10, "before_sequence": 999}),
                    validated: true,
                    validation_id: None,
                },
                &ToolExecutionContext::new(directory.path().canonicalize().unwrap()),
            )
            .await;

        assert!(!result.is_error);
        assert!(result.output.contains("边界内消息"));
        assert!(!result.output.contains("边界外消息"));
        assert_eq!(inner.execute_calls.load(Ordering::SeqCst), 0);
        assert!(inner
            .contexts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty());
        assert!(executor
            .list_tools()
            .iter()
            .any(|tool| tool.name == READ_GROUP_MESSAGES_TOOL));
    }
}
