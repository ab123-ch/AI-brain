pub mod config;
pub mod error;
pub mod openai_compat;
pub mod provider;
pub mod stream;
pub mod types;

pub use config::LlmConfig;
pub use error::{LlmError, Result};
pub use openai_compat::OpenAiCompatClient;
pub use provider::{ChatMessage, ChatRequest, ChatResponse, LlmProvider, MessageRole};
pub use types::{ContentBlock, FinishReason, StreamEvent, ToolChoice, ToolDefinition, TokenUsage};
