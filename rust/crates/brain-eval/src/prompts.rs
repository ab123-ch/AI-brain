use std::fmt::Write;

use brain_core::types::{EvalRequirement, EvolutionRule, PitfallCategory, PitfallRecord, TurnRecord, TurnRole, UserProfile};
use chrono::{Datelike, Utc};

use crate::extractor::{FileChange, FileChangeType};
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
    let cwd = std::env::current_dir()
        .map_or_else(|_| "unknown".into(), |p| p.display().to_string());
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

/// 构建评估系统提示词（Skill 化版本）
///
/// 四段式结构：角色定义 + 环境信息 + available_skills + 用户评估要求 + 输出格式
pub fn build_evaluation_system_prompt(
    eval_requirements: &[EvalRequirement],
    registry: &SkillRegistry,
    with_tools: bool,
) -> String {
    let mut prompt = String::new();

    // 第一段：角色定义
    prompt.push_str(r"# 角色定义

你是 AI Brain 系统的质量审核员，负责在主脑产生输出后评估其质量和安全性。

## 核心能力
- **任务完成度审查**：判断主脑是否真正完成了用户要求的任务
- **安全风险识别**：检测代码中的危险操作和安全隐患
- **事实正确性校验**：验证日期、技术细节是否准确（以环境信息为准）
- **偏好合规检查**：确认输出遵守用户的偏好和禁忌（只在直接相关时）

");

    // 如果有工具能力，添加工作方式说明
    if with_tools {
        prompt.push_str(r"## 工作方式
- 你可以调用 Skill 工具加载具体的审查规则
- 你可以使用只读工具（read_file、grep、bash）验证代码
- 只报告确实存在的问题，宁可漏报不误报
- 不确定的事实不要标记为错误

");
    } else {
        prompt.push_str(r"## 工作方式
- 只报告确实存在的问题，宁可漏报不误报
- 不确定的事实不要标记为错误

");
    }

    // 第二段：环境信息
    prompt.push_str(&build_environment_info());
    prompt.push('\n');

    // 第三段：available_skills
    prompt.push_str(&build_available_skills(registry));

    // Skill tool 说明
    if with_tools {
        prompt.push_str(r#"# Skill 工具

你可以调用 Skill 工具加载具体审查规则：
- `Skill("code-verification")` — 代码变更验证（编译、测试、空实现、日志、需求匹配）
- `Skill("conclusion-verification")` — 结论真实性验证（证据验证、逻辑链检查）

加载后你将看到完整的检查维度和铁律。

"#);
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
    prompt.push_str(r"# 输出格式

没有问题时，严格输出（不要附加其他文字）：
评估结果-正常

有问题时，严格输出（不要附加其他文字）：
评估结果-存在问题。具体问题：1.问题描述及修正建议 2.问题描述及修正建议 ...

注意：不要输出 JSON，不要使用代码块，只输出纯文本。");

    prompt
}

/// 构建评估用户提示词
///
/// 将踩坑库 + 用户画像 + 自进化规则 + 主脑输入输出注入上下文
#[allow(clippy::cognitive_complexity)]
pub fn build_evaluation_user_prompt(
    user_input: &str,
    ai_output: &str,
    pitfalls: &[PitfallRecord],
    user_profile: &UserProfile,
    rules: &[EvolutionRule],
    file_changes: &[FileChange],
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

    // 文件修改记录
    let changes_text = format_file_changes(file_changes);
    if !changes_text.is_empty() {
        prompt.push_str(&changes_text);
    }

    // 踩坑库
    if !pitfalls.is_empty() {
        prompt.push_str("## 踩坑库（已知错误模式）\n");
        for (i, p) in pitfalls.iter().enumerate() {
            let _ = writeln!(
                prompt,
                "{}. [{}] {} (出现{}次)",
                i + 1,
                category_label(p.category),
                p.description,
                p.occurrence_count
            );
            if let Some(ref correction) = p.user_correction {
                let _ = write!(prompt, " — 用户纠正: {correction}");
            }
            prompt.push('\n');
        }
        prompt.push('\n');
    }

    // 用户画像
    let has_profile = !user_profile.explicit_preferences.is_empty()
        || !user_profile.implicit_preferences.is_empty()
        || !user_profile.taboos.is_empty()
        || !user_profile.habits.is_empty();

    if has_profile {
        prompt.push_str("## 用户画像\n");

        if !user_profile.explicit_preferences.is_empty() {
            prompt.push_str("### 显性偏好\n");
            for pref in &user_profile.explicit_preferences {
                let _ = writeln!(prompt, "- {pref}");
            }
        }

        if !user_profile.implicit_preferences.is_empty() {
            prompt.push_str("### 隐性偏好\n");
            for pref in &user_profile.implicit_preferences {
                let _ = writeln!(prompt, "- {pref}");
            }
        }

        if !user_profile.taboos.is_empty() {
            prompt.push_str("### 禁忌（绝对不能做的事）\n");
            for taboo in &user_profile.taboos {
                let _ = writeln!(prompt, "- {taboo}");
            }
        }

        if !user_profile.habits.is_empty() {
            prompt.push_str("### 习惯\n");
            for habit in &user_profile.habits {
                let _ = writeln!(prompt, "- {habit}");
            }
        }

        prompt.push('\n');
    }

    // 自进化规则
    if !rules.is_empty() {
        prompt.push_str("## 自进化规则（避坑指南）\n");
        for (i, r) in rules.iter().enumerate() {
            let _ = writeln!(prompt, "{}. [优先级{}] {}", i + 1, r.priority, r.rule);
        }
        prompt.push('\n');
    }

    prompt.push_str("请根据以上信息评估主脑输出。没有问题输出「评估结果-正常」，有问题输出「评估结果-存在问题。具体问题：...」");

    prompt
}

/// 将文件变更列表格式化为 prompt 文本
pub fn format_file_changes(changes: &[FileChange]) -> String {
    if changes.is_empty() {
        return String::new();
    }

    let mut s = String::from("## 文件修改记录（主脑本轮操作）\n\n");
    for (i, c) in changes.iter().enumerate() {
        match c.change_type {
            FileChangeType::Edit => {
                let _ = writeln!(s, "{}. 编辑 `{}`", i + 1, c.file_path);
                if let Some(ref old) = c.old_content {
                    let truncated: String = old.chars().take(500).collect();
                    let _ = writeln!(s, "   替换前: {truncated}");
                }
                if let Some(ref new) = c.new_content {
                    let truncated: String = new.chars().take(500).collect();
                    let _ = writeln!(s, "   替换后: {truncated}");
                }
            }
            FileChangeType::Write => {
                let _ = writeln!(s, "{}. 写入 `{}`（新建或覆盖）", i + 1, c.file_path);
            }
        }
    }
    s.push('\n');
    s
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

        let _ = writeln!(s, "{}. [{}] → {}({}ms)", i + 1, tc.tool_name, status, tc.duration_ms);

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

fn category_label(category: PitfallCategory) -> &'static str {
    match category {
        PitfallCategory::ToolFailure => "工具失败",
        PitfallCategory::WrongAnswer => "答案错误",
        PitfallCategory::FormatIssue => "格式问题",
        PitfallCategory::LazyBehavior => "偷懒行为",
        PitfallCategory::Other => "其他",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::PitfallCategory;
    use chrono::Utc;

    #[test]
    fn system_prompt_has_role_definition() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false);
        assert!(prompt.contains("角色定义"));
        assert!(prompt.contains("质量审核员"));
        assert!(prompt.contains("核心能力"));
    }

    #[test]
    fn system_prompt_has_fixed_dimensions() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), false);
        // 新版本的系统提示词使用"核心能力"替代"评估维度"
        assert!(prompt.contains("任务完成度审查"));
        assert!(prompt.contains("安全风险识别"));
        assert!(prompt.contains("事实正确性校验"));
        assert!(prompt.contains("偏好合规检查"));
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
        // 新版本的系统提示词在"工作方式"中包含判定原则
        assert!(prompt.contains("宁可漏报不误报"));
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
            &UserProfile::default(),
            &[],
            &[],
        );
        assert!(prompt.contains("帮我写一个函数"));
        assert!(prompt.contains("fn add"));
        assert!(prompt.contains("请根据以上信息评估"));
    }

    #[test]
    fn user_prompt_with_pitfalls() {
        let pitfall = PitfallRecord {
            id: "p1".into(),
            category: PitfallCategory::LazyBehavior,
            description: "使用了 unwrap()".into(),
            user_correction: Some("应该用 ok_or".into()),
            occurred_at: Utc::now(),
            occurrence_count: 3,
            superseded: false,
        };
        let prompt = build_evaluation_user_prompt(
            "写代码",
            "some code",
            &[pitfall],
            &UserProfile::default(),
            &[],
            &[],
        );
        assert!(prompt.contains("踩坑库"));
        assert!(prompt.contains("unwrap"));
        assert!(prompt.contains("用户纠正"));
        assert!(prompt.contains("出现3次"));
    }

    #[test]
    fn user_prompt_with_user_profile() {
        let mut profile = UserProfile::default();
        profile
            .explicit_preferences
            .push("使用 Rust 惯用写法".into());
        profile.taboos.push("禁止使用 unsafe".into());

        let prompt = build_evaluation_user_prompt("写代码", "some code", &[], &profile, &[], &[]);
        assert!(prompt.contains("用户画像"));
        assert!(prompt.contains("显性偏好"));
        assert!(prompt.contains("Rust 惯用写法"));
        assert!(prompt.contains("禁忌"));
        assert!(prompt.contains("unsafe"));
    }

    #[test]
    fn user_prompt_with_evolution_rules() {
        let rule = EvolutionRule {
            id: "r1".into(),
            rule: "永远不要在循环里分配内存".into(),
            source_pitfall_ids: vec!["p1".into()],
            priority: 5,
            created_at: Utc::now(),
            superseded: false,
        };
        let prompt = build_evaluation_user_prompt(
            "写代码",
            "some code",
            &[],
            &UserProfile::default(),
            &[rule],
            &[],
        );
        assert!(prompt.contains("自进化规则"));
        assert!(prompt.contains("优先级5"));
        assert!(prompt.contains("循环里分配内存"));
    }

    #[test]
    fn user_prompt_no_profile_section_when_empty() {
        let prompt =
            build_evaluation_user_prompt("input", "output", &[], &UserProfile::default(), &[], &[]);
        assert!(!prompt.contains("用户画像"));
    }

    #[test]
    fn category_label_matches() {
        assert_eq!(category_label(PitfallCategory::ToolFailure), "工具失败");
        assert_eq!(category_label(PitfallCategory::WrongAnswer), "答案错误");
        assert_eq!(category_label(PitfallCategory::FormatIssue), "格式问题");
        assert_eq!(category_label(PitfallCategory::LazyBehavior), "偷懒行为");
        assert_eq!(category_label(PitfallCategory::Other), "其他");
    }

    #[test]
    fn system_prompt_with_tools_has_verification_section() {
        let prompt = build_evaluation_system_prompt(&[], &SkillRegistry::new(), true);
        // 新版本的系统提示词使用 Skill 工具说明替代验证工具
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
    fn user_prompt_with_file_changes() {
        let changes = vec![
            FileChange {
                file_path: "src/main.rs".into(),
                change_type: FileChangeType::Edit,
                old_content: Some("fn old()".into()),
                new_content: Some("fn new() {}".into()),
            },
        ];
        let prompt = build_evaluation_user_prompt(
            "改代码", "已修改", &[], &UserProfile::default(), &[], &changes,
        );
        assert!(prompt.contains("文件修改记录"));
        assert!(prompt.contains("src/main.rs"));
        assert!(prompt.contains("fn new()"));
    }

    #[test]
    fn user_prompt_without_file_changes_no_section() {
        let prompt = build_evaluation_user_prompt(
            "闲聊", "你好", &[], &UserProfile::default(), &[], &[],
        );
        assert!(!prompt.contains("文件修改记录"));
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
        let turns = vec![
            make_assistant("hello"),
            make_assistant("world"),
        ];
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
