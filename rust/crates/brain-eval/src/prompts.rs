use std::fmt::Write;

use brain_core::types::{EvolutionRule, PitfallCategory, PitfallRecord, UserProfile};

/// 构建评估系统提示词
///
/// 评估脑的身份声明 + 五项检查规则 + 判定标准 + JSON 输出格式
pub fn build_evaluation_system_prompt() -> String {
    r#"你是 AI Brain 系统的评估脑，负责在主脑产生输出后自动评估其质量。

## 身份
- 你是常驻后台的监听者，每次主脑输出后自动触发
- 你的任务是检查主脑输出是否存在问题
- 你基于记忆脑提供的踩坑库、用户画像、自进化规则进行判断

## 五项检查
1. **重复踩坑** — 主脑输出是否重复了已知的错误模式？
   注意：只有主脑犯了技术/逻辑错误才算踩坑。用户反复问同一问题不等于踩坑。
2. **用户偏好违反** — 是否违反了用户的显性偏好、隐性偏好或习惯？
3. **已知失败模式** — 是否重复了自进化规则中总结的避坑指南的反面？
4. **偷懒行为** — 是否用 TODO/FIXME/省略号代替了实现？
5. **事实正确性** — 输出中的事实声明是否可能不正确？

## 判定标准（非常重要）
- 踩坑 = 主脑犯了技术/逻辑错误（代码bug、错误事实、用unsafe替代安全方案）
- 踩坑 ≠ 用户反复问同一问题、用户测试系统、用户闲聊确认
- 如果踩坑记录描述的是"用户行为"而非"主脑技术错误"，不算踩坑复现
- 只有确信主脑输出有问题才报告，宁可漏报也不要误报

## 规则
- 只报告确实存在的问题，不要过度敏感
- 如果没有发现问题，返回 {"passed": true, "issues": []}
- severity 判断标准：
  - Critical: 违反禁忌、重复踩坑（技术错误）、偷懒行为、事实错误
  - Warning: 轻微的风格问题、可能但不确定的问题
- suggestion 必须给出具体的修正建议

## 输出格式
严格输出 JSON，不要附加其他文字：
{
  "passed": true/false,
  "issues": [
    {
      "severity": "Warning" 或 "Critical",
      "category": "PitfallRepeat" | "PreferenceViolation" | "KnownFailurePattern" | "LazyBehavior" | "FactError" | "InstructionIgnored",
      "description": "具体问题描述",
      "suggestion": "给主脑的修正建议"
    }
  ]
}"#
        .to_string()
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

    prompt.push_str("请根据以上信息评估主脑输出，严格按照 JSON 格式返回结果。");

    prompt
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
    fn system_prompt_contains_five_checks() {
        let prompt = build_evaluation_system_prompt();
        assert!(prompt.contains("重复踩坑"));
        assert!(prompt.contains("用户偏好"));
        assert!(prompt.contains("失败模式"));
        assert!(prompt.contains("偷懒"));
        assert!(prompt.contains("事实"));
    }

    #[test]
    fn system_prompt_contains_json_format() {
        let prompt = build_evaluation_system_prompt();
        assert!(prompt.contains("passed"));
        assert!(prompt.contains("severity"));
        assert!(prompt.contains("suggestion"));
    }

    #[test]
    fn system_prompt_contains_anti_false_positive_rules() {
        let prompt = build_evaluation_system_prompt();
        assert!(prompt.contains("判定标准"));
        assert!(prompt.contains("用户反复问同一问题不等于踩坑"));
        assert!(prompt.contains("宁可漏报也不要误报"));
    }

    #[test]
    fn user_prompt_basic_input_output() {
        let prompt = build_evaluation_user_prompt(
            "帮我写一个函数",
            "fn add(a: i32, b: i32) -> i32 { a + b }",
            &[],
            &UserProfile::default(),
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
        };
        let prompt = build_evaluation_user_prompt(
            "写代码",
            "some code",
            &[pitfall],
            &UserProfile::default(),
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

        let prompt = build_evaluation_user_prompt("写代码", "some code", &[], &profile, &[]);
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
        };
        let prompt = build_evaluation_user_prompt(
            "写代码",
            "some code",
            &[],
            &UserProfile::default(),
            &[rule],
        );
        assert!(prompt.contains("自进化规则"));
        assert!(prompt.contains("优先级5"));
        assert!(prompt.contains("循环里分配内存"));
    }

    #[test]
    fn user_prompt_no_profile_section_when_empty() {
        let prompt =
            build_evaluation_user_prompt("input", "output", &[], &UserProfile::default(), &[]);
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
}
