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
        source_event_seq: u64,
    ) -> Self {
        Self {
            repository,
            room_id: room_id.into(),
            context_through_seq: context_through_seq.min(source_event_seq.saturating_sub(1)),
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
        description: "按需查看当前群聊在本次消息之前的用户和实例消息。仅当请求依赖其他实例或公共讨论时使用；只能读取当前群，且不会返回冻结边界之后的消息。".into(),
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
        let before_sequence = tool_call
            .input
            .get("before_sequence")
            .and_then(serde_json::Value::as_u64);
        let through_sequence = before_sequence
            .map(|value| maximum_sequence.min(value.saturating_sub(1)))
            .unwrap_or(maximum_sequence);

        Box::pin(async move {
            let started = Instant::now();
            let result = repository
                .conversation_events_through(&room_id, through_sequence, requested_limit)
                .map_err(|error| error.to_string())
                .and_then(|page| {
                    let next_before_sequence = page
                        .has_more
                        .then(|| page.events.first().map(|event| event.sequence))
                        .flatten();
                    tracing::info!(
                        room_id = %room_id,
                        requested_limit,
                        returned_messages = page.events.len(),
                        before_sequence = ?before_sequence,
                        context_through_sequence = maximum_sequence,
                        has_more = page.has_more,
                        "成员按需读取群消息"
                    );
                    serde_json::to_string_pretty(&GroupMessageToolOutput {
                        room_id: room_id.clone(),
                        context_through_sequence: maximum_sequence,
                        has_more: page.has_more,
                        next_before_sequence,
                        messages: page
                            .events
                            .iter()
                            .map(GroupMessageToolEvent::from)
                            .collect(),
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
    has_more: bool,
    next_before_sequence: Option<u64>,
    messages: Vec<GroupMessageToolEvent>,
}

#[derive(Serialize)]
struct GroupMessageToolEvent {
    event_id: String,
    sequence: u64,
    sender_kind: String,
    sender_id: String,
    sender_name: String,
    kind: String,
    parent_event_id: Option<String>,
    conversation_root_event_id: String,
    content: String,
}

impl From<&RoomEventView> for GroupMessageToolEvent {
    fn from(event: &RoomEventView) -> Self {
        Self {
            event_id: event.event_id.clone(),
            sequence: event.sequence,
            sender_kind: event.sender_kind.clone(),
            sender_id: event.sender_id.clone(),
            sender_name: event.sender_name.clone(),
            kind: event.kind.clone(),
            parent_event_id: event.parent_event_id.clone(),
            conversation_root_event_id: event.conversation_root_event_id.clone(),
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
            GroupMessageToolScope::new(repository, "room-1", 0, 1),
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
        let second = repository
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
            GroupMessageToolScope::new(
                Arc::clone(&repository),
                "room-1",
                second.event.sequence,
                second.event.sequence,
            ),
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
        let output: serde_json::Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(
            output["context_through_sequence"],
            serde_json::json!(first.event.sequence)
        );
        assert_eq!(output["has_more"], serde_json::json!(false));
        assert!(output["next_before_sequence"].is_null());
        assert_eq!(output["messages"][0]["event_id"], first.event.event_id);
        assert_eq!(
            output["messages"][0]["sender_id"],
            serde_json::json!("user")
        );
        assert_eq!(
            output["messages"][0]["conversation_root_event_id"],
            first.event.conversation_root_event_id
        );
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

    #[tokio::test]
    async fn scoped_tool_filters_internal_events_and_paginates_without_overlap() {
        let directory = tempfile::tempdir().unwrap();
        let repository =
            Arc::new(CollaborationRepository::new(directory.path(), Default::default()).unwrap());
        let room = repository.ensure_room("room-1", "Tool Room", &[]).unwrap();
        repository
            .create_member("room-1", "智脑 B", None, None)
            .unwrap();
        for index in 1..=5 {
            repository
                .post_group_message(
                    "room-1",
                    std::slice::from_ref(&room.room.default_member_id),
                    &format!("公共消息 {index}"),
                    RoomInputMode::Chat,
                    &format!("tool-page-{index}"),
                )
                .unwrap();
        }
        let current = repository
            .post_group_message(
                "room-1",
                std::slice::from_ref(&room.room.default_member_id),
                "当前消息",
                RoomInputMode::Chat,
                "tool-page-current",
            )
            .unwrap();
        let executor = GroupMessageToolExecutor::new(
            Arc::new(RecordingToolExecutor::default()),
            GroupMessageToolScope::new(
                Arc::clone(&repository),
                "room-1",
                current.event.sequence,
                current.event.sequence,
            ),
        );
        let context = ToolExecutionContext::new(directory.path().canonicalize().unwrap());
        let page = |before_sequence: Option<u64>| ToolCall {
            tool_name: READ_GROUP_MESSAGES_TOOL.into(),
            input: before_sequence.map_or_else(
                || serde_json::json!({"limit": 2}),
                |cursor| serde_json::json!({"limit": 2, "before_sequence": cursor}),
            ),
            validated: true,
            validation_id: None,
        };

        let first_result = executor.execute_with_context(&page(None), &context).await;
        assert!(!first_result.is_error);
        assert!(!first_result.output.contains("member_created"));
        assert!(!first_result.output.contains("当前消息"));
        let first: serde_json::Value = serde_json::from_str(&first_result.output).unwrap();
        assert_eq!(first["has_more"], serde_json::json!(true));
        let cursor = first["next_before_sequence"].as_u64().unwrap();
        let first_ids = first["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["event_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(first_ids.len(), 2);
        assert!(first["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|message| message["kind"] == "user_message"));
        let second = executor
            .execute_with_context(&page(Some(cursor)), &context)
            .await;
        assert!(!second.is_error);
        let second: serde_json::Value = serde_json::from_str(&second.output).unwrap();
        let second_ids = second["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["event_id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(second_ids.len(), 2);
        assert!(first_ids.iter().all(|id| !second_ids.contains(id)));
        assert!(second["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|message| message["sequence"].as_u64().unwrap() < cursor));
    }
}
