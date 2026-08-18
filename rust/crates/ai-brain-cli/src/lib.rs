//! AI Brain CLI reusable application modules.
//!
//! The binary target and integration tests share this library so the
//! orchestrator is compiled and exercised as one implementation.

pub mod api_server;
pub mod command;
pub(crate) mod command_execution;
pub mod config_manager;
pub mod init;
pub mod llm_usage_logger;
pub mod novel_adapters;
pub mod orchestrator;
pub(crate) mod query_context;
pub mod real_tool_executor;
pub mod remote_access;
pub mod repl;
pub mod runtime_trace;
pub mod tui;
pub mod web;
pub(crate) mod workspace_changes;
