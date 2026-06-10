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

/// 构建评估系统提示词（副脑验证版本）
///
/// 核心理念：副脑验证主脑结论的正确性，加载技能路由表，注入用户画像和踩坑记录。
/// 四步评估逻辑：识别任务类型 → 加载对应Skill → 证据搜集验证 → 用户要求合规检查
pub fn build_evaluation_system_prompt(
    eval_requirements: &[EvalRequirement],
    registry: &SkillRegistry,
    with_tools: bool,
    profile_summary: Option<&str>,
    pitfall_descriptions: Option<&[String]>,
) -> String {
    let mut prompt = String::new();

    // 第一段：角色定义 + 四步法
    prompt.push_str(
        r"# 角色定义

你是主脑的副脑。你的唯一职责是验证主脑的结论是否正确、是否有数据支撑、是否违反用户要求。

你不做需求满足度检查——那是主脑自己的事。你只关注两件事：
1. 主脑的结论是否经得起验证（有数据/代码/日志佐证）
2. 主脑是否违反了用户在各会话中明确或隐含的要求

## 评估流程（四步法）

### 第一步：识别任务类型
从主脑的操作轨迹判断主脑做了什么类型的任务。

### 第二步：加载对应审查技能
按技能路由表调用 Skill 工具，获取该任务类型的审查规则。

### 第三步：证据搜集验证
调用只读工具（read_file、grep_search、bash）搜集佐证数据。
验证主脑的每个事实性断言——能证实的标记为已验证，能证伪的标记为问题。
**用户提出的问题，主脑的回答必须有事实性数据/案例/代码支撑。**

### 第四步：用户要求合规检查
对照用户画像（禁忌/习惯/偏好）+ 踩坑记录 + 用户评估要求。
检查主脑输出是否违反了任何用户要求。

",
    );

    // 工具说明
    if with_tools {
        prompt.push_str(
            r"## 工具使用
- 你可以调用 Skill 工具加载具体的审查规则
- 你可以使用只读工具（read_file、grep_search、bash）验证代码和事实
- 不确定的事实用工具验证，不凭感觉判断

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

    // 用户画像注入
    if let Some(summary) = profile_summary {
        if !summary.is_empty() {
            prompt.push_str("\n# 用户画像\n\n");
            prompt.push_str(summary);
            prompt.push_str("\n");
        }
    }

    // 踩坑记录注入
    if let Some(pitfalls) = pitfall_descriptions {
        if !pitfalls.is_empty() {
            prompt.push_str("\n# 踩坑记录（主脑不能重复犯的错误）\n\n");
            for (i, desc) in pitfalls.iter().enumerate() {
                let _ = writeln!(prompt, "{}. {}", i + 1, desc);
            }
        }
    }

    prompt.push('\n');

    // 第三段：available_skills
    prompt.push_str(&build_available_skills(registry));

    // Skill tool 说明
    if with_tools {
        prompt.push_str(
            r#"# Skill 工具

你可以调用 Skill 工具加载具体审查规则：
- `Skill("troubleshooting-verification")` — 排查问题结论验证
- `Skill("code-verification")` — 代码变更验证
- `Skill("writing-verification")` — 写作内容评估
- `Skill("conclusion-verification")` — 结论真实性验证

先根据技能路由表判断任务类型，再加载对应 Skill。

"#,
        );
    }

    // 技能路由表
    prompt.push_str(
        r"## 技能路由表

根据主脑操作轨迹判断任务类型，加载对应 Skill：

| 任务类型 | 判断依据 | 加载的 Skill |
|---------|---------|-------------|
| 排查问题 | 主脑调用了 grep/read_file/bash 查日志、查链路、查配置 | troubleshooting-verification |
| 代码修改 | 主脑调用了 edit_file/write_file | code-verification |
| 写作/创作 | 主脑输出了长文本（>500字），无 edit_file/write_file | writing-verification |
| 通用问答 | 简短回答、事实性断言、其他类型 | conclusion-verification |

**重要：用户提出的问题必须走深度验证。** 主脑回答中必须有事实性数据、案例、代码等支撑，不能只有推理。

",
    );

    // 第四段：用户评估要求
    if !eval_requirements.is_empty() {
        prompt.push_str("# 用户评估要求\n\n");
        prompt.push_str("以下是用户对评估的具体要求和纠正，请严格遵循：\n\n");
        for (i, req) in eval_requirements.iter().enumerate() {
            let _ = writeln!(prompt, "{}. {}", i + 1, req.content);
        }
        prompt.push_str("\n**用户评估要求优先级高于固定评估维度。**\n\n");
    }

    // 评估原则
    prompt.push_str(
        r"## 评估原则
- **验证优先**：不确定的事实用工具验证，不凭感觉判断
- **用户要求至上**：用户定义的禁忌和规则必须严格执行
- **宁漏勿报**：不确定的问题不报，但确定的问题必须报

",
    );

    // 严重程度说明
    prompt.push_str(
        r"## 严重程度判定

- Critical（必须通知用户）：事实性错误、结论被证伪、违反用户禁忌、重复踩坑
- Warning（自动修正）：非关键建议、风格问题、非最佳实践

输出格式中用 [Critical] 或 [Warning] 标记每个问题的严重程度。

",
    );

    // 输出格式
    prompt.push_str(
        r"# 输出格式

没有问题时，严格输出（不要附加其他文字）：
评估结果-正常

有问题时，严格输出（不要附加其他文字）：
评估结果-存在问题。具体问题：1.[Critical/Warning] 问题描述及修正建议 2.[Critical/Warning] 问题描述及修正建议 ...

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

    prompt.push_str("请按照四步法评估：1)识别任务类型 → 2)加载对应Skill → 3)证据搜集验证 → 4)用户要求合规检查。没有问题输出「评估结果-正常」，有问题输出「评估结果-存在问题。具体问题：...」");

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
    fn system_prompt_has_vice_brain_role() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false, None, None);
        assert!(prompt.contains("副脑"));
    }

    #[test]
    fn system_prompt_has_skill_routing() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false, None, None);
        assert!(prompt.contains("技能路由表"));
        assert!(prompt.contains("troubleshooting-verification"));
        assert!(prompt.contains("writing-verification"));
    }

    #[test]
    fn system_prompt_has_four_steps() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false, None, None);
        assert!(prompt.contains("四步法"));
        assert!(prompt.contains("识别任务类型"));
        assert!(prompt.contains("证据搜集验证"));
        assert!(prompt.contains("用户要求合规检查"));
    }

    #[test]
    fn system_prompt_no_old_three_steps() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false, None, None);
        assert!(!prompt.contains("三步法"));
        assert!(!prompt.contains("理解用户需求"));
        assert!(!prompt.contains("对照主脑输出"));
        assert!(!prompt.contains("二次校验"));
    }

    #[test]
    fn system_prompt_no_quality_auditor() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false, None, None);
        assert!(!prompt.contains("质量审核员"));
    }

    #[test]
    fn system_prompt_has_evaluation_principles() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false, None, None);
        assert!(prompt.contains("验证优先"));
        assert!(prompt.contains("用户要求至上"));
        assert!(prompt.contains("宁漏勿报"));
    }

    #[test]
    fn system_prompt_has_severity_levels() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false, None, None);
        assert!(prompt.contains("严重程度判定"));
        assert!(prompt.contains("Critical"));
        assert!(prompt.contains("Warning"));
    }

    #[test]
    fn system_prompt_with_profile_summary() {
        let prompt = build_evaluation_system_prompt(
            &[],
            &SkillRegistry::new(),
            false,
            Some("用户偏好 Rust，禁忌使用 unwrap"),
            None,
        );
        assert!(prompt.contains("# 用户画像"));
        assert!(prompt.contains("禁忌使用 unwrap"));
    }

    #[test]
    fn system_prompt_without_profile_summary() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false, None, None);
        // 没有传入 profile_summary 时，不应出现 "# 用户画像" 标题
        assert!(!prompt.contains("# 用户画像"));
    }

    #[test]
    fn system_prompt_with_empty_profile_summary() {
        let prompt =
            build_evaluation_system_prompt(&[], &SkillRegistry::new(), false, Some(""), None);
        // 空字符串 profile_summary 时，不应出现 "# 用户画像" 标题
        assert!(!prompt.contains("# 用户画像"));
    }

    #[test]
    fn system_prompt_with_pitfall_descriptions() {
        let pitfalls = vec!["使用 unwrap 导致 panic".to_string()];
        let prompt = build_evaluation_system_prompt(
            &[],
            &SkillRegistry::new(),
            false,
            None,
            Some(&pitfalls),
        );
        assert!(prompt.contains("# 踩坑记录"));
        assert!(prompt.contains("使用 unwrap 导致 panic"));
    }

    #[test]
    fn system_prompt_without_pitfall_descriptions() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false, None, None);
        // 没有传入 pitfall_descriptions 时，不应出现 "# 踩坑记录" 标题
        assert!(!prompt.contains("# 踩坑记录"));
    }

    #[test]
    fn system_prompt_with_empty_pitfall_descriptions() {
        let pitfalls: Vec<String> = vec![];
        let prompt = build_evaluation_system_prompt(
            &[],
            &SkillRegistry::new(),
            false,
            None,
            Some(&pitfalls),
        );
        // 空列表时，不应出现 "# 踩坑记录" 标题
        assert!(!prompt.contains("# 踩坑记录"));
    }

    #[test]
    fn system_prompt_has_output_format() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false, None, None);
        assert!(prompt.contains("评估结果-正常"));
        assert!(prompt.contains("评估结果-存在问题"));
        assert!(prompt.contains("不要输出 JSON"));
    }

    #[test]
    fn system_prompt_output_format_has_severity_tags() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false, None, None);
        assert!(prompt.contains("[Critical/Warning]"));
    }

    #[test]
    fn system_prompt_no_preference_check() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false, None, None);
        assert!(!prompt.contains("偏好合规检查"));
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
        let prompt =
            build_evaluation_system_prompt(&reqs, &SkillRegistry::new(), false, None, None);
        assert!(prompt.contains("# 用户评估要求"));
        assert!(prompt.contains("不要将简单问答判定为问题"));
        assert!(prompt.contains("重点关注代码安全性"));
        assert!(prompt.contains("优先级高于固定评估维度"));
    }

    #[test]
    fn system_prompt_without_eval_requirements() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false, None, None);
        // 没有传入 eval_requirements 时，不应出现 "# 用户评估要求" 标题
        assert!(!prompt.contains("# 用户评估要求"));
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
    }

    #[test]
    fn user_prompt_uses_four_steps() {
        let prompt = build_evaluation_user_prompt("测试", "输出", &[]);
        assert!(prompt.contains("四步法"));
        assert!(!prompt.contains("三步法"));
    }

    #[test]
    fn user_prompt_no_user_profile() {
        // 确保用户 prompt 中不包含用户画像标题
        let prompt = build_evaluation_user_prompt("写代码", "some code", &[]);
        assert!(!prompt.contains("# 用户画像"));
        assert!(!prompt.contains("显性偏好"));
        assert!(!prompt.contains("隐性偏好"));
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
    fn system_prompt_with_tools_has_skill_section() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), true, None, None);
        assert!(prompt.contains("# Skill 工具"));
        assert!(prompt.contains("troubleshooting-verification"));
        assert!(prompt.contains("code-verification"));
        assert!(prompt.contains("writing-verification"));
        assert!(prompt.contains("conclusion-verification"));
    }

    #[test]
    fn system_prompt_without_tools_no_skill_section() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false, None, None);
        // 没有 with_tools=true 时，不应出现 "# Skill 工具" 标题
        assert!(!prompt.contains("# Skill 工具"));
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
        let prompt = build_evaluation_user_prompt("改代码", "已修改", &turns);
        assert!(prompt.contains("主脑操作轨迹"));
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("fn new()"));
    }

    #[test]
    fn user_prompt_without_file_changes_no_section() {
        let prompt = build_evaluation_user_prompt("闲聊", "你好", &[]);
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
