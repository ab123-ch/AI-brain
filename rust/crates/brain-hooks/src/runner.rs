use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tracing::warn;

use crate::builtins;
use crate::config::HooksConfig;
use crate::types::{HookDecision, HookHandlerConfig, HookInput, HookOutput, Matcher};

/// Hook 执行引擎
pub struct HookRunner {
    config: HooksConfig,
}

impl std::fmt::Debug for HookRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookRunner")
            .field("config", &self.config)
            .finish()
    }
}

impl HookRunner {
    pub fn new(mut config: HooksConfig) -> Self {
        // 自动注册 eval_gate builtin handler 到 post_query 列表
        if config.eval_gate.enabled {
            let already_registered = config
                .post_query
                .iter()
                .any(|h| matches!(h, HookHandlerConfig::Builtin { name } if name == "eval_gate"));
            if !already_registered {
                config.post_query.insert(
                    0,
                    HookHandlerConfig::Builtin {
                        name: "eval_gate".to_string(),
                    },
                );
            }
        }
        Self { config }
    }

    /// 获取配置引用
    pub fn config(&self) -> &HooksConfig {
        &self.config
    }

    /// 主入口：运行某个事件的所有 handler
    pub async fn run(&self, input: &HookInput) -> Vec<HookOutput> {
        // 全局开关关闭 → 直接放行
        if !self.config.enabled {
            return vec![HookOutput::allow()];
        }

        let handlers = self.config.handlers_for_event(input.event);

        // 无 handler → 直接放行
        if handlers.is_empty() {
            return vec![HookOutput::allow()];
        }

        let mut outputs = Vec::new();

        for handler in handlers {
            let result = self.execute_handler(handler, input).await;

            match result {
                Some(output) => {
                    let is_deny = output.decision == HookDecision::Deny;
                    outputs.push(output);

                    // Deny 短路：停止后续 handler
                    if is_deny {
                        break;
                    }
                }
                None => {
                    // 执行失败：warn 但不阻断，视为 Allow
                    warn!(
                        event = %input.event.as_str(),
                        "Hook handler execution failed, treating as Allow"
                    );
                }
            }
        }

        // 如果所有 handler 都失败了（outputs 为空），返回默认放行
        if outputs.is_empty() {
            outputs.push(HookOutput::allow());
        }

        outputs
    }

    /// 执行单个 handler
    async fn execute_handler(
        &self,
        handler: &HookHandlerConfig,
        input: &HookInput,
    ) -> Option<HookOutput> {
        match handler {
            HookHandlerConfig::Command {
                command,
                matcher,
                timeout,
            } => {
                self.execute_command(command, matcher.as_deref(), *timeout, input)
                    .await
            }
            HookHandlerConfig::Builtin { name } => match name.as_str() {
                "eval_gate" => {
                    let user_input = input.user_input.clone().unwrap_or_default();
                    let mode = self.config.eval_gate.mode;
                    // 从 ai_output 中解析工具名列表（由 Orchestrator 以 __tool_names:...__ 前缀注入）
                    let tool_names = extract_tool_names(input.ai_output.as_deref());
                    Some(builtins::run_eval_gate(&user_input, mode, &tool_names))
                }
                _ => {
                    warn!(builtin = %name, "Unknown builtin handler");
                    None
                }
            },
        }
    }

    /// 执行 command handler
    async fn execute_command(
        &self,
        command: &str,
        matcher: Option<&str>,
        timeout_secs: u64,
        input: &HookInput,
    ) -> Option<HookOutput> {
        // Matcher 过滤：对工具事件检查 tool_name 是否匹配
        if let Some(tool_name) = &input.tool_name {
            let m = Matcher::parse(matcher);
            if !m.matches(tool_name) {
                // 不匹配则跳过，视为 Allow
                return Some(HookOutput::allow());
            }
        }

        let command = command.to_string();
        let event_str = input.event.as_str().to_string();
        let session_id = input.session_id.clone();
        let cwd = input.cwd.clone();
        let tool_name = input.tool_name.clone();
        let payload = build_payload(input);
        let payload_str = serde_json::to_string(&payload).unwrap_or_default();
        let duration = Duration::from_secs(timeout_secs);

        tokio::task::spawn_blocking(move || {
            run_shell_command(
                &command,
                &event_str,
                &session_id,
                &cwd,
                &tool_name,
                &payload_str,
                duration,
            )
        })
        .await
        .ok()? // JoinError → None
    }
}

/// 从 ai_output 中提取工具名列表
///
/// Orchestrator 在 ai_output 前缀注入 `__tool_names:Edit,Write,Bash__\n`
/// 本函数解析该前缀并返回工具名列表
fn extract_tool_names(ai_output: Option<&str>) -> Vec<String> {
    let output = match ai_output {
        Some(s) => s,
        None => return Vec::new(),
    };

    if let Some(rest) = output.strip_prefix("__tool_names:") {
        if let Some(end) = rest.find("__") {
            return rest[..end]
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
        }
    }

    Vec::new()
}

/// 构建 JSON payload
fn build_payload(input: &HookInput) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "hook_event_name": input.event.as_str(),
        "session_id": input.session_id,
    });

    if let Some(ref v) = input.tool_name {
        payload["tool_name"] = serde_json::json!(v);
    }
    if let Some(ref v) = input.tool_input {
        payload["tool_input"] = serde_json::json!(v);
    }
    if let Some(ref v) = input.tool_output {
        payload["tool_output"] = serde_json::json!(v);
    }
    if let Some(ref v) = input.user_input {
        payload["user_input"] = serde_json::json!(v);
    }
    if let Some(ref v) = input.ai_output {
        payload["ai_output"] = serde_json::json!(v);
    }

    payload
}

/// 在 spawn_blocking 内执行 shell 命令
fn run_shell_command(
    command: &str,
    event_str: &str,
    session_id: &str,
    cwd: &std::path::Path,
    tool_name: &Option<String>,
    payload: &str,
    timeout: Duration,
) -> Option<HookOutput> {
    let mut child = match Command::new("sh")
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HOOK_EVENT", event_str)
        .env("HOOK_SESSION_ID", session_id)
        .env("HOOK_TOOL_NAME", tool_name.as_deref().unwrap_or(""))
        .env(
            "HOOK_TOOL_INPUT",
            tool_name.as_ref().map(|_| "1").unwrap_or(""),
        )
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "Failed to spawn hook command");
            return None;
        }
    };

    // 写入 stdin
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(payload.as_bytes()) {
            warn!(error = %e, "Failed to write to hook stdin");
        }
    }
    // drop stdin to close it, signaling EOF to the child

    // 超时控制：用辅助线程做超时 kill
    let child_arc = Arc::new(Mutex::new(Some(child)));
    let timeout_arc = child_arc.clone();

    let timeout_handle = thread::spawn(move || {
        thread::sleep(timeout);
        if let Ok(mut guard) = timeout_arc.lock() {
            if let Some(ref mut c) = *guard {
                let _ = c.kill();
            }
        }
    });

    // 等待子进程完成：从 Mutex<Option<Child>> 中 take 出来，再 wait_with_output
    let output = match child_arc.lock() {
        Ok(mut guard) => match guard.take() {
            Some(c) => c.wait_with_output(),
            None => {
                warn!("Child process already taken");
                return None;
            }
        },
        Err(e) => {
            warn!(error = %e, "Failed to lock child process");
            return None;
        }
    };

    // 不需要等 timeout 线程，它会在 kill 后自然结束或 child 已退出时无效
    let _ = timeout_handle.join();

    match output {
        Ok(out) => {
            let exit_code = out.status.code().unwrap_or(-1);

            // 退出码语义：0 = Allow, 2 = Deny, 其他 = Failed
            match exit_code {
                0 => {
                    // 尝试从 stdout 解析 JSON 获取更多字段
                    let stdout_str = String::from_utf8_lossy(&out.stdout);
                    Some(parse_hook_output(stdout_str.trim(), HookDecision::Allow))
                }
                2 => {
                    // Deny
                    let stdout_str = String::from_utf8_lossy(&out.stdout);
                    let mut hook_out = parse_hook_output(stdout_str.trim(), HookDecision::Deny);
                    if hook_out.reason.is_none() {
                        hook_out.reason = Some("Hook denied execution".to_string());
                    }
                    Some(hook_out)
                }
                _ => {
                    // 非零退出码（超时 kill 后也是非零）
                    let stderr_str = String::from_utf8_lossy(&out.stderr);
                    warn!(
                        exit_code = exit_code,
                        stderr = %stderr_str,
                        "Hook command exited with non-zero status"
                    );
                    None
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "Failed to wait for hook command");
            None
        }
    }
}

/// 解析 hook 的 stdout JSON 输出
fn parse_hook_output(stdout: &str, default_decision: HookDecision) -> HookOutput {
    if stdout.is_empty() {
        return HookOutput {
            decision: default_decision,
            reason: None,
            trigger_eval: false,
            system_message: None,
        };
    }

    // 尝试解析 JSON
    match serde_json::from_str::<serde_json::Value>(stdout) {
        Ok(val) => {
            let decision = match val.get("decision").and_then(|v| v.as_str()) {
                Some("block" | "deny" | "Deny") => HookDecision::Deny,
                Some("allow" | "Allow") => HookDecision::Allow,
                _ => default_decision,
            };

            let reason = val.get("reason").and_then(|v| v.as_str()).map(String::from);

            let system_message = val
                .get("systemMessage")
                .and_then(|v| v.as_str())
                .map(String::from);

            let trigger_eval = val
                .get("trigger_eval")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            HookOutput {
                decision,
                reason,
                trigger_eval,
                system_message,
            }
        }
        Err(_) => {
            // 非 JSON 输出，使用默认决策
            HookOutput {
                decision: default_decision,
                reason: if !stdout.is_empty() {
                    Some(stdout.to_string())
                } else {
                    None
                },
                trigger_eval: false,
                system_message: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EvalGateConfig, EvalGateMode};
    use crate::types::HookEvent;

    fn make_input(event: HookEvent) -> HookInput {
        HookInput {
            event,
            session_id: "test-session".to_string(),
            cwd: std::path::PathBuf::from("/tmp"),
            tool_name: None,
            tool_input: None,
            tool_output: None,
            is_error: false,
            user_input: None,
            ai_output: None,
        }
    }

    #[tokio::test]
    async fn test_disabled_returns_allow() {
        let config = HooksConfig {
            enabled: false,
            ..Default::default()
        };
        let runner = HookRunner::new(config);
        let input = make_input(HookEvent::PreToolUse);
        let outputs = runner.run(&input).await;
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].decision, HookDecision::Allow);
    }

    #[tokio::test]
    async fn test_no_handlers_returns_allow() {
        let runner = HookRunner::new(HooksConfig::default());
        let input = make_input(HookEvent::OnShutdown);
        let outputs = runner.run(&input).await;
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].decision, HookDecision::Allow);
    }

    #[tokio::test]
    async fn test_deny_short_circuits() {
        let config = HooksConfig {
            enabled: true,
            pre_tool_use: vec![
                HookHandlerConfig::Command {
                    command: "echo '{\"decision\":\"deny\",\"reason\":\"blocked\"}'".to_string(),
                    matcher: None,
                    timeout: 5,
                },
                HookHandlerConfig::Command {
                    command: "echo 'should not run'".to_string(),
                    matcher: None,
                    timeout: 5,
                },
            ],
            ..Default::default()
        };
        let runner = HookRunner::new(config);
        let input = make_input(HookEvent::PreToolUse);
        let outputs = runner.run(&input).await;
        // Deny 短路，只运行第一个 handler
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].decision, HookDecision::Deny);
        assert_eq!(outputs[0].reason.as_deref(), Some("blocked"));
    }

    #[tokio::test]
    async fn test_allow_command() {
        let config = HooksConfig {
            enabled: true,
            eval_gate: EvalGateConfig {
                enabled: false,
                mode: EvalGateMode::OnFileEdit,
            },
            post_query: vec![HookHandlerConfig::Command {
                command: "echo '{\"decision\":\"allow\"}'".to_string(),
                matcher: None,
                timeout: 5,
            }],
            ..Default::default()
        };
        let runner = HookRunner::new(config);
        let input = make_input(HookEvent::PostQuery);
        let outputs = runner.run(&input).await;
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].decision, HookDecision::Allow);
    }

    #[tokio::test]
    async fn test_exit_code_0_is_allow() {
        let config = HooksConfig {
            enabled: true,
            session_start: vec![HookHandlerConfig::Command {
                command: "exit 0".to_string(),
                matcher: None,
                timeout: 5,
            }],
            ..Default::default()
        };
        let runner = HookRunner::new(config);
        let input = make_input(HookEvent::SessionStart);
        let outputs = runner.run(&input).await;
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].decision, HookDecision::Allow);
    }

    #[tokio::test]
    async fn test_exit_code_2_is_deny() {
        let config = HooksConfig {
            enabled: true,
            pre_tool_use: vec![HookHandlerConfig::Command {
                command: "exit 2".to_string(),
                matcher: None,
                timeout: 5,
            }],
            ..Default::default()
        };
        let runner = HookRunner::new(config);
        let input = make_input(HookEvent::PreToolUse);
        let outputs = runner.run(&input).await;
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].decision, HookDecision::Deny);
    }

    #[tokio::test]
    async fn test_failed_command_warns_not_blocks() {
        let config = HooksConfig {
            enabled: true,
            pre_tool_use: vec![HookHandlerConfig::Command {
                command: "exit 1".to_string(),
                matcher: None,
                timeout: 5,
            }],
            ..Default::default()
        };
        let runner = HookRunner::new(config);
        let input = make_input(HookEvent::PreToolUse);
        let outputs = runner.run(&input).await;
        // 失败的 handler 不应该产出 output，但也不是 deny
        // 结果应该包含默认的 allow（因为所有 handler 都"失败"了）
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].decision, HookDecision::Allow);
    }

    #[tokio::test]
    async fn test_matcher_skips_non_matching_tool() {
        let config = HooksConfig {
            enabled: true,
            pre_tool_use: vec![HookHandlerConfig::Command {
                command: "echo '{\"decision\":\"deny\"}'".to_string(),
                matcher: Some("Edit|Write".to_string()),
                timeout: 5,
            }],
            ..Default::default()
        };
        let runner = HookRunner::new(config);
        let mut input = make_input(HookEvent::PreToolUse);
        input.tool_name = Some("Bash".to_string());
        let outputs = runner.run(&input).await;
        // Bash 不匹配 Edit|Write，handler 跳过（返回 allow）
        assert_eq!(outputs[0].decision, HookDecision::Allow);
    }

    #[tokio::test]
    async fn test_matcher_matches_tool() {
        let config = HooksConfig {
            enabled: true,
            pre_tool_use: vec![HookHandlerConfig::Command {
                command: "echo '{\"decision\":\"deny\",\"reason\":\"no edits\"}'".to_string(),
                matcher: Some("Edit|Write".to_string()),
                timeout: 5,
            }],
            ..Default::default()
        };
        let runner = HookRunner::new(config);
        let mut input = make_input(HookEvent::PreToolUse);
        input.tool_name = Some("Edit".to_string());
        let outputs = runner.run(&input).await;
        assert_eq!(outputs[0].decision, HookDecision::Deny);
    }

    #[test]
    fn test_build_payload_with_all_fields() {
        let input = HookInput {
            event: HookEvent::PreToolUse,
            session_id: "sess-1".to_string(),
            cwd: std::path::PathBuf::from("/tmp"),
            tool_name: Some("Bash".to_string()),
            tool_input: Some("ls -la".to_string()),
            tool_output: Some("file.txt".to_string()),
            is_error: false,
            user_input: None,
            ai_output: None,
        };
        let payload = build_payload(&input);
        assert_eq!(payload["hook_event_name"], "PreToolUse");
        assert_eq!(payload["session_id"], "sess-1");
        assert_eq!(payload["tool_name"], "Bash");
        assert_eq!(payload["tool_input"], "ls -la");
        assert_eq!(payload["tool_output"], "file.txt");
        assert!(payload.get("user_input").is_none());
    }

    #[test]
    fn test_parse_hook_output_json_block() {
        let out = parse_hook_output(
            r#"{"decision":"block","reason":"dangerous","trigger_eval":true}"#,
            HookDecision::Allow,
        );
        assert_eq!(out.decision, HookDecision::Deny);
        assert_eq!(out.reason.as_deref(), Some("dangerous"));
        assert!(out.trigger_eval);
    }

    #[test]
    fn test_parse_hook_output_non_json() {
        let out = parse_hook_output("some plain text", HookDecision::Allow);
        assert_eq!(out.decision, HookDecision::Allow);
        assert_eq!(out.reason.as_deref(), Some("some plain text"));
    }

    #[test]
    fn test_parse_hook_output_empty() {
        let out = parse_hook_output("", HookDecision::Allow);
        assert_eq!(out.decision, HookDecision::Allow);
        assert!(out.reason.is_none());
    }
}
