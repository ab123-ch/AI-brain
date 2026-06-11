use crate::config::EvalGateMode;
use crate::types::HookOutput;

/// 运行 eval_gate：根据模式和工具调用历史判断是否需要评估
///
/// - `Always`:       非 trivial input 一律触发
/// - `OnFileEdit`:   仅当 turns 中包含文件修改工具调用时触发（默认）
/// - `Never`:        从不触发
pub fn run_eval_gate(user_input: &str, mode: EvalGateMode, tool_names: &[String]) -> HookOutput {
    match mode {
        EvalGateMode::Never => {
            tracing::info!("eval_gate: mode=Never，跳过评估");
            HookOutput::allow()
        }
        EvalGateMode::Always => {
            if is_trivial_input(user_input) {
                tracing::info!("eval_gate: mode=Always, trivial input，跳过评估");
                return HookOutput::allow();
            }
            tracing::info!("eval_gate: mode=Always, 需要评估");
            HookOutput {
                decision: crate::types::HookDecision::Allow,
                reason: None,
                trigger_eval: true,
                system_message: None,
            }
        }
        EvalGateMode::OnFileEdit => {
            if has_file_edit_tool(tool_names) {
                tracing::info!(
                    "eval_gate: mode=OnFileEdit, 检测到文件修改工具({:?}), 需要评估",
                    tool_names
                );
                HookOutput {
                    decision: crate::types::HookDecision::Allow,
                    reason: None,
                    trigger_eval: true,
                    system_message: None,
                }
            } else {
                tracing::debug!("eval_gate: mode=OnFileEdit, 无文件修改工具, 跳过评估");
                HookOutput::allow()
            }
        }
    }
}

/// 判断工具调用列表中是否包含文件修改类工具
fn has_file_edit_tool(tool_names: &[String]) -> bool {
    /// 文件修改类工具名列表
    const FILE_EDIT_TOOLS: &[&str] = &[
        "Edit",
        "Write",
        "edit",
        "write",
        "apply_refactor_tool",
        // Bash 中含文件操作的命令模式在调用时由 tool_name 区分
    ];

    tool_names.iter().any(|name| {
        FILE_EDIT_TOOLS
            .iter()
            .any(|&t| name == t || name.contains(t))
    })
}

/// 纯规则判断是否为 trivial input（无需评估）
fn is_trivial_input(user_input: &str) -> bool {
    let input_trimmed = user_input.trim();
    let input_len = input_trimmed.chars().count();

    // 极短输入（<=6字符）且匹配常见打招呼/确认模式
    if input_len <= 6 {
        let lower = input_trimmed.to_lowercase();
        let trivial_patterns = [
            "你好", "hi", "hello", "hey", "嗨", "哈喽", "ok", "好的", "谢谢", "thanks", "嗯", "哦",
            "不是",
        ];
        if trivial_patterns.iter().any(|p| lower.contains(p)) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trivial_input_short_greeting() {
        assert!(is_trivial_input("你好"));
        assert!(is_trivial_input("hi"));
        assert!(is_trivial_input("hello"));
        assert!(is_trivial_input("好的"));
    }

    #[test]
    fn trivial_input_short_confirmation() {
        assert!(is_trivial_input("嗯嗯"));
        assert!(is_trivial_input("ok"));
    }

    #[test]
    fn non_trivial_input_real_question() {
        assert!(!is_trivial_input("帮我写一个 Rust 函数计算阶乘"));
        assert!(!is_trivial_input("今天天气怎么样"));
        assert!(!is_trivial_input("请分析一下这段代码的问题"));
    }

    #[test]
    fn non_trivial_input_medium_length() {
        assert!(!is_trivial_input("帮我写一个排序算法，要求时间复杂度O(n)"));
    }

    // ── Always 模式 ──

    #[test]
    fn eval_gate_always_trivial_skips() {
        let output = run_eval_gate("你好", EvalGateMode::Always, &[]);
        assert!(!output.trigger_eval);
    }

    #[test]
    fn eval_gate_always_real_triggers() {
        let output = run_eval_gate("帮我写代码", EvalGateMode::Always, &[]);
        assert!(output.trigger_eval);
    }

    // ── OnFileEdit 模式 ──

    #[test]
    fn eval_gate_on_file_edit_no_tools_skips() {
        let output = run_eval_gate("帮我写代码", EvalGateMode::OnFileEdit, &[]);
        assert!(!output.trigger_eval);
    }

    #[test]
    fn eval_gate_on_file_edit_with_edit_tool_triggers() {
        let output = run_eval_gate(
            "帮我写代码",
            EvalGateMode::OnFileEdit,
            &["Edit".to_string()],
        );
        assert!(output.trigger_eval);
    }

    #[test]
    fn eval_gate_on_file_edit_with_write_tool_triggers() {
        let output = run_eval_gate(
            "帮我写代码",
            EvalGateMode::OnFileEdit,
            &["Write".to_string()],
        );
        assert!(output.trigger_eval);
    }

    #[test]
    fn eval_gate_on_file_edit_with_bash_only_skips() {
        let output = run_eval_gate(
            "帮我写代码",
            EvalGateMode::OnFileEdit,
            &["Bash".to_string()],
        );
        assert!(!output.trigger_eval);
    }

    #[test]
    fn eval_gate_on_file_edit_mixed_tools_triggers() {
        let output = run_eval_gate(
            "帮我写代码",
            EvalGateMode::OnFileEdit,
            &["Bash".to_string(), "Read".to_string(), "Edit".to_string()],
        );
        assert!(output.trigger_eval);
    }

    // ── Never 模式 ──

    #[test]
    fn eval_gate_never_always_skips() {
        let output = run_eval_gate("帮我写代码", EvalGateMode::Never, &["Edit".to_string()]);
        assert!(!output.trigger_eval);
    }

    // ── has_file_edit_tool ──

    #[test]
    fn test_has_file_edit_tool() {
        assert!(has_file_edit_tool(&["Edit".to_string()]));
        assert!(has_file_edit_tool(&["Write".to_string()]));
        assert!(has_file_edit_tool(&["apply_refactor_tool".to_string()]));
        assert!(!has_file_edit_tool(&["Bash".to_string()]));
        assert!(!has_file_edit_tool(&["Read".to_string()]));
        assert!(!has_file_edit_tool(&["Grep".to_string()]));
        assert!(!has_file_edit_tool(&[]));
    }
}
