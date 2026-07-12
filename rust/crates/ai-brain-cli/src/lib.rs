//! AI Brain CLI reusable application modules.
//!
//! The binary target and integration tests share this library so the
//! orchestrator is compiled and exercised as one implementation.

pub mod api_server;
pub mod command;
pub mod config_manager;
pub mod init;
pub mod llm_usage_logger;
pub mod orchestrator;
pub mod real_tool_executor;
pub mod repl;
pub mod runtime_trace;
pub mod tui;
pub mod web;
