use crate::types::HookOutput;

/// 运行 eval_gate：纯规则判断是否需要评估
///
/// 策略：打招呼等 trivial input 直接跳过，其余一律触发评估脑
pub fn run_eval_gate(user_input: &str) -> HookOutput {
    if is_trivial_input(user_input) {
        tracing::info!("eval_gate: trivial input，跳过评估");
        return HookOutput::allow();
    }

    tracing::info!("eval_gate: 需要评估");
    HookOutput {
        decision: crate::types::HookDecision::Allow,
        reason: None,
        trigger_eval: true,
        system_message: None,
    }
}

/// 纯规则判断是否为 trivial input（无需评估）
fn is_trivial_input(user_input: &str) -> bool {
    let input_trimmed = user_input.trim();
    let input_len = input_trimmed.chars().count();

    // 极短输入（<=6字符）且匹配常见打招呼/确认模式
    if input_len <= 6 {
        let lower = input_trimmed.to_lowercase();
        let trivial_patterns = [
            "你好", "hi", "hello", "hey", "嗨", "哈喽", "ok", "好的",
            "谢谢", "thanks", "嗯", "哦", "不是",
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

    #[test]
    fn run_eval_gate_trivial_skips() {
        let output = run_eval_gate("你好");
        assert!(!output.trigger_eval);
    }

    #[test]
    fn run_eval_gate_real_triggers() {
        let output = run_eval_gate("帮我写代码");
        assert!(output.trigger_eval);
    }
}
