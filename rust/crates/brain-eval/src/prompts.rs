use std::fmt::Write;

use brain_core::types::{EvalRequirement, TurnRecord, TurnRole};
use chrono::{Datelike, Utc};

use crate::skills::SkillRegistry;

/// 构建运行环境信息段（注入 system prompt）
///
/// 让评估脑感知当前日期，避免幻觉错误时间。
pub fn build_environment_info() -> String {
    let os = match std::env::consts::OS {
        "macos" => "macOS",
        "linux" => "Linux",
        "windows" => "Windows",
        other => other,
    };
    let cwd =
        std::env::current_dir().map_or_else(|_| "unknown".into(), |p| p.display().to_string());
    let now = Utc::now();
    let date_str = now.format("%Y年%m月%d日").to_string();
    let weekday = match now.weekday().num_days_from_monday() {
        0 => "周一",
        1 => "周二",
        2 => "周三",
        3 => "周四",
        4 => "周五",
        5 => "周六",
        6 => "周日",
        _ => "未知",
    };
    format!("\n## 运行环境\n- 操作系统: {os}\n- 工作目录: {cwd}\n- 当前日期: {date_str} {weekday}")
}

/// 构建 available_skills XML 段
///
/// 将所有 skill 的 name + description 格式化为 XML，
/// 嵌入 system prompt，供 LLM 自然路由。
pub fn build_available_skills(registry: &SkillRegistry) -> String {
    let skills = registry.all_skills();
    if skills.is_empty() {
        return String::new();
    }

    let mut s = String::from("\n<available_skills>\n");
    for skill in skills {
        let _ = writeln!(s, "  <skill>");
        let _ = writeln!(s, "    <name>{}</name>", skill.name);
        let _ = writeln!(s, "    <description>{}</description>", skill.description);
        let _ = writeln!(s, "  </skill>");
    }
    s.push_str("</available_skills>\n");
    s
}

/// 构建评估系统提示词（需求驱动版本）
///
/// 核心理念：评估脑聚焦当前用户需求，而非历史记忆。
/// 三步评估逻辑：理解需求 → 对照输出 → 二次校验
pub fn build_evaluation_system_prompt(
    eval_requirements: &[EvalRequirement],
    registry: &SkillRegistry,
    with_tools: bool,
) -> String {
    let mut prompt = String::new();

    // 第一段：角色定义 + 评估方法论
    prompt.push_str(
        r"# 角色定义

你是 AI Brain 系统的质量审核员。你的唯一职责是验证主脑的输出是否真正满足了用户的当前需求。

## 评估方法论（三步法）

你必须严格按以下三步进行评估，不得跳步：

### 第一步：理解用户需求
仔细分析用户的原始输入，拆解出：
- 核心意图：用户到底想要什么
- 具体要求：有哪些明确的约束、条件、格式要求
- 隐含期望：从需求上下文可以合理推断的期望（必须与当前需求直接相关）

### 第二步：对照主脑输出
将主脑的输出与第一步理解的需求逐条对照：
- 是否完成了用户要求的核心任务？
- 是否满足所有明确约束？
- 代码/结论中是否存在安全风险或事实性错误？
- 是否存在偷懒行为（TODO占位、省略实现等）？

### 第三步：二次校验（关键）
如果第二步发现问题，**必须进行二次校验**：
- 重新审视用户原始需求，确认自己的理解是否正确
- 如果是自己理解有误 → 输出「评估结果-正常」
- 如果确认主脑确实有问题 → 输出具体问题和修正建议

## 评估原则
- **只关注当前需求**：不参考任何用户的历史偏好、习惯、画像
- **宁漏勿报**：只报告确实存在的问题，不确定的不报
- **不吹毛求疵**：风格、措辞、称呼等不涉及任务正确性的细节不评估
- **不重复劳动**：主脑已完成的任务不要要求换一种方式重做

",
    );

    // 工具说明
    if with_tools {
        prompt.push_str(
            r"## 工具使用
- 你可以调用 Skill 工具加载具体的审查规则
- 你可以使用只读工具（read_file、grep、bash）验证代码
- 不确定的事实不要标记为错误，可以用工具验证后再判断

",
        );
    } else {
        prompt.push_str(
            r"## 注意事项
- 不确定的事实不要标记为错误

",
        );
    }

    // 第二段：环境信息
    prompt.push_str(&build_environment_info());
    prompt.push('\n');

    // 第三段：available_skills
    prompt.push_str(&build_available_skills(registry));

    // Skill tool 说明
    if with_tools {
        prompt.push_str(
            r#"# Skill 工具

你可以调用 Skill 工具加载具体审查规则：
- `Skill("code-verification")` — 代码变更验证（编译、测试、空实现、日志、需求匹配）
- `Skill("conclusion-verification")` — 结论真实性验证（证据验证、逻辑链检查）

加载后你将看到完整的检查维度和铁律。

"#,
        );
    }

    // 第四段：用户评估要求
    if !eval_requirements.is_empty() {
        prompt.push_str("# 用户评估要求\n\n");
        prompt.push_str("以下是用户对评估的具体要求和纠正，请严格遵循：\n\n");
        for (i, req) in eval_requirements.iter().enumerate() {
            let _ = writeln!(prompt, "{}. {}", i + 1, req.content);
        }
        prompt.push_str("\n**用户评估要求优先级高于固定评估维度。**\n\n");
    }

    // 输出格式
    prompt.push_str(
        r"# 输出格式

没有问题时，严格输出（不要附加其他文字）：
评估结果-正常

有问题时，严格输出（不要附加其他文字）：
评估结果-存在问题。具体问题：1.问题描述及修正建议 2.问题描述及修正建议 ...

注意：不要输出 JSON，不要使用代码块，只输出纯文本。",
    );

    prompt
}

/// 构建评估用户提示词
///
/// 聚焦当前需求：用户输入 + 主脑输出 + 操作轨迹
/// 不再注入用户画像、踩坑库、进化规则等历史记忆数据
pub fn build_evaluation_user_prompt(
    user_input: &str,
    ai_output: &str,
    turns: &[TurnRecord],
) -> String {
    let mut prompt = String::new();

    // 用户输入
    prompt.push_str("## 用户输入\n");
    prompt.push_str(user_input);
    prompt.push_str("\n\n");

    // 主脑输出
    prompt.push_str("## 主脑输出（待评估）\n");
    prompt.push_str(ai_output);
    prompt.push_str("\n\n");

    // 主脑操作轨迹（工具调用）
    let trace_text = format_tool_trace(turns);
    if !trace_text.is_empty() {
        prompt.push_str(&trace_text);
    }

    prompt.push_str("请按照三步法评估：1)理解用户需求 → 2)对照主脑输出 → 3)二次校验。没有问题输出「评估结果-正常」，有问题输出「评估结果-存在问题。具体问题：...」");

    prompt
}

/// 将主脑所有工具调用格式化为摘要文本，注入评估脑 prompt
///
/// 只处理 `TurnRole::ToolCall` 且 `tool_call` 为 Some 的记录，
/// 格式化为：序号 + 工具名 + 成功/失败 + 耗时(ms) + 输入/输出摘要
pub fn format_tool_trace(turns: &[TurnRecord]) -> String {
    let tool_calls: Vec<_> = turns
        .iter()
        .filter(|t| matches!(t.role, TurnRole::ToolCall) && t.tool_call.is_some())
        .collect();

    if tool_calls.is_empty() {
        return String::new();
    }

    let mut s = String::from("## 主脑操作轨迹\n\n");
    for (i, turn) in tool_calls.iter().enumerate() {
        let tc = turn.tool_call.as_ref().unwrap();
        let status = if tc.is_error { "失败" } else { "成功" };

        let _ = writeln!(
            s,
            "{}. [{}] → {}({}ms)",
            i + 1,
            tc.tool_name,
            status,
            tc.duration_ms
        );

        // 输入 JSON
        let input_json = serde_json::to_string(&tc.input).unwrap_or_else(|_| tc.input.to_string());
        let _ = writeln!(s, "   输入: {input_json}");

        if tc.is_error {
            // 失败：完整错误信息
            let _ = writeln!(s, "   错误: {}", tc.output);
        } else {
            // 成功：根据工具类型差异化展示
            match tc.tool_name.as_str() {
                "edit_file" => {
                    if let Some(old) = tc.input.get("old_string").and_then(|v| v.as_str()) {
                        let truncated: String = old.chars().take(500).collect();
                        let _ = writeln!(s, "   替换前: {truncated}");
                    }
                    if let Some(new) = tc.input.get("new_string").and_then(|v| v.as_str()) {
                        let truncated: String = new.chars().take(500).collect();
                        let _ = writeln!(s, "   替换后: {truncated}");
                    }
                }
                "write_file" => {
                    if let Some(content) = tc.input.get("content").and_then(|v| v.as_str()) {
                        let truncated: String = content.chars().take(500).collect();
                        let _ = writeln!(s, "   内容: {truncated}");
                    }
                }
                _ => {
                    // 其他工具：输出摘要前 300 字符
                    let truncated: String = tc.output.chars().take(300).collect();
                    if tc.output.chars().count() > 300 {
                        let _ = writeln!(s, "   输出摘要: {truncated}...");
                    } else {
                        let _ = writeln!(s, "   输出摘要: {truncated}");
                    }
                }
            }
        }
    }

    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn system_prompt_has_role_definition() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false);
        assert!(prompt.contains("角色定义"));
        assert!(prompt.contains("质量审核员"));
        assert!(prompt.contains("三步法"));
    }

    #[test]
    fn system_prompt_has_evaluation_dimensions() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false);
        // 需求驱动版本不再有"偏好合规检查"，改为三步法
        assert!(prompt.contains("理解用户需求"));
        assert!(prompt.contains("对照主脑输出"));
        assert!(prompt.contains("二次校验"));
    }

    #[test]
    fn system_prompt_no_preference_check() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false);
        // 确保不再包含"偏好合规检查"
        assert!(!prompt.contains("偏好合规检查"));
    }

    #[test]
    fn system_prompt_has_output_format() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false);
        assert!(prompt.contains("评估结果-正常"));
        assert!(prompt.contains("评估结果-存在问题"));
        assert!(prompt.contains("不要输出 JSON"));
    }

    #[test]
    fn system_prompt_has_judgment_principles() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false);
        assert!(prompt.contains("宁漏勿报"));
        assert!(prompt.contains("不吹毛求疵"));
    }

    #[test]
    fn system_prompt_with_eval_requirements() {
        let reqs = vec![
            EvalRequirement {
                id: "evreq-1".into(),
                content: "不要将简单问答判定为问题".into(),
                source: "用户反馈".into(),
                created_at: Utc::now(),
                superseded: false,
            },
            EvalRequirement {
                id: "evreq-2".into(),
                content: "重点关注代码安全性".into(),
                source: "记忆脑分析".into(),
                created_at: Utc::now(),
                superseded: false,
            },
        ];
        let prompt = build_evaluation_system_prompt(&reqs, &SkillRegistry::new(), false);
        assert!(prompt.contains("用户评估要求"));
        assert!(prompt.contains("不要将简单问答判定为问题"));
        assert!(prompt.contains("重点关注代码安全性"));
        assert!(prompt.contains("优先级高于固定评估维度"));
    }

    #[test]
    fn system_prompt_without_eval_requirements() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false);
        assert!(!prompt.contains("用户评估要求"));
    }

    #[test]
    fn user_prompt_basic_input_output() {
        let prompt = build_evaluation_user_prompt(
            "帮我写一个函数",
            "fn add(a: i32, b: i32) -> i32 { a + b }",
            &[],
        );
        assert!(prompt.contains("帮我写一个函数"));
        assert!(prompt.contains("fn add"));
        assert!(prompt.contains("三步法"));
    }

    #[test]
    fn user_prompt_no_user_profile() {
        // 确保不再包含用户画像相关内容
        let prompt = build_evaluation_user_prompt("写代码", "some code", &[]);
        assert!(!prompt.contains("用户画像"));
        assert!(!prompt.contains("显性偏好"));
        assert!(!prompt.contains("隐性偏好"));
        assert!(!prompt.contains("禁忌"));
    }

    #[test]
    fn user_prompt_no_pitfalls() {
        // 确保不再包含踩坑库
        let prompt = build_evaluation_user_prompt("写代码", "some code", &[]);
        assert!(!prompt.contains("踩坑库"));
    }

    #[test]
    fn user_prompt_no_evolution_rules() {
        // 确保不再包含自进化规则
        let prompt = build_evaluation_user_prompt("写代码", "some code", &[]);
        assert!(!prompt.contains("自进化规则"));
    }

    #[test]
    fn system_prompt_with_tools_has_verification_section() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), true);
        assert!(prompt.contains("Skill 工具"));
        assert!(prompt.contains("code-verification"));
        assert!(prompt.contains("conclusion-verification"));
    }

    #[test]
    fn system_prompt_without_tools_no_verification_section() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false);
        assert!(!prompt.contains("Skill 工具"));
    }

    #[test]
    fn user_prompt_with_file_changes_shows_trace() {
        use brain_core::types::{ToolCallRecord, TurnRole};
        let turns = vec![TurnRecord {
            role: TurnRole::ToolCall,
            content: String::new(),
            tool_call: Some(ToolCallRecord {
                tool_name: "edit_file".into(),
                input: serde_json::json!({
                    "file_path": "src/main.rs",
                    "old_string": "fn old()",
                    "new_string": "fn new() {}"
                }),
                output: "OK".into(),
                duration_ms: 10,
                is_error: false,
            }),
            timestamp: String::new(),
        }];
        let prompt = build_evaluation_user_prompt(
            "改代码",
            "已修改",
            &turns,
        );
        assert!(prompt.contains("主脑操作轨迹"));
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("fn new()"));
    }

    #[test]
    fn user_prompt_without_file_changes_no_section() {
        let prompt =
            build_evaluation_user_prompt("闲聊", "你好", &[]);
        assert!(!prompt.contains("主脑操作轨迹"));
    }

    // ─── format_tool_trace 测试 ──────────────────────────────────

    use brain_core::types::{ToolCallRecord, TurnRecord, TurnRole};

    fn make_tool_call(
        tool_name: &str,
        input: serde_json::Value,
        output: &str,
        duration_ms: u64,
        is_error: bool,
    ) -> TurnRecord {
        TurnRecord {
            role: TurnRole::ToolCall,
            content: String::new(),
            tool_call: Some(ToolCallRecord {
                tool_name: tool_name.into(),
                input,
                output: output.into(),
                duration_ms,
                is_error,
            }),
            timestamp: "2026-05-20T00:00:00Z".into(),
        }
    }

    fn make_assistant(content: &str) -> TurnRecord {
        TurnRecord {
            role: TurnRole::Assistant,
            content: content.into(),
            tool_call: None,
            timestamp: "2026-05-20T00:00:00Z".into(),
        }
    }

    #[test]
    fn format_tool_trace_shows_all_tool_types() {
        let turns = vec![
            make_assistant("正在查询天气..."),
            make_tool_call(
                "bash",
                serde_json::json!({"command": "curl -s wttr.in/Ningbo"}),
                "Weather report: Ningbo...Partly Cloudy +24°C",
                1249,
                false,
            ),
            make_tool_call(
                "bash",
                serde_json::json!({"command": "curl -s \"wttr.in/Ningbo?format=...\""}),
                "安全拒绝: Bash command contains potentially destructive pattern: \"format\"",
                0,
                true,
            ),
        ];

        let trace = format_tool_trace(&turns);

        // 标题存在
        assert!(trace.contains("主脑操作轨迹"));
        // bash 成功
        assert!(trace.contains("[bash] → 成功(1249ms)"));
        assert!(trace.contains("Weather report"));
        // bash 失败
        assert!(trace.contains("[bash] → 失败(0ms)"));
        assert!(trace.contains("安全拒绝"));
        // Assistant 角色不应出现
        assert!(!trace.contains("正在查询天气"));
    }

    #[test]
    fn format_tool_trace_empty_returns_empty() {
        let trace = format_tool_trace(&[]);
        assert!(trace.is_empty());
    }

    #[test]
    fn format_tool_trace_only_assistant_returns_empty() {
        let turns = vec![make_assistant("hello"), make_assistant("world")];
        let trace = format_tool_trace(&turns);
        assert!(trace.is_empty());
    }

    #[test]
    fn format_tool_trace_output_truncated() {
        let long_output: String = "X".repeat(500);
        let turns = vec![make_tool_call(
            "grep",
            serde_json::json!({"pattern": "TODO"}),
            &long_output,
            100,
            false,
        )];

        let trace = format_tool_trace(&turns);

        // 输出摘要应被截断到 300 字符 + "..."
        assert!(trace.contains("输出摘要:"));
        assert!(trace.contains("..."));
        // 不应包含完整的 500 字符输出
        assert!(!trace.contains(&long_output));
    }

    #[test]
    fn format_tool_trace_edit_file_shows_old_new() {
        let turns = vec![make_tool_call(
            "edit_file",
            serde_json::json!({
                "file_path": "src/main.rs",
                "old_string": "fn old()",
                "new_string": "fn new() {}"
            }),
            "File edited successfully",
            10,
            false,
        )];

        let trace = format_tool_trace(&turns);

        assert!(trace.contains("[edit_file] → 成功(10ms)"));
        assert!(trace.contains("替换前: fn old()"));
        assert!(trace.contains("替换后: fn new() {}"));
        // 不应出现"输出摘要"
        assert!(!trace.contains("输出摘要"));
    }
}
