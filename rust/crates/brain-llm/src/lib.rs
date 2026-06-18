pub mod config;
pub mod echo;
pub mod error;
pub mod gemini;
pub mod http_client;
pub mod openai_compat;
pub mod provider;
pub mod stream;
pub mod types;

pub use config::{LlmConfig, ProviderKind, ProxySection};
pub use error::{LlmError, Result};
pub use gemini::GeminiClient;
pub use openai_compat::{OpenAiCompatClient, RetryConfig};
pub use provider::{
    build_context_messages, ChatMessage, ChatRequest, ChatResponse, LlmProvider, MessageRole,
};
pub use types::{ContentBlock, FinishReason, StreamEvent, TokenUsage, ToolChoice, ToolDefinition};
