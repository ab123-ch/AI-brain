pub mod config;
pub mod echo;
pub mod error;
pub mod gemini;
pub mod http_client;
pub mod openai_compat;
pub mod provider;
pub mod retry;
pub mod stream;
pub mod types;

pub use config::{LlmConfig, ProviderKind, ProxySection};
pub use error::{LlmError, Result};
pub use gemini::GeminiClient;
pub use openai_compat::OpenAiCompatClient;
pub use provider::{
    build_context_messages, ChatMessage, ChatRequest, ChatResponse, LlmProvider, MessageRole,
};
pub use retry::RetryConfig;
pub use types::{ContentBlock, FinishReason, StreamEvent, TokenUsage, ToolChoice, ToolDefinition};
