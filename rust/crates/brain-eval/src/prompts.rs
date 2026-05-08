use std::fmt::Write;

use brain_core::types::{EvalRequirement, EvolutionRule, PitfallCategory, PitfallRecord, UserProfile};

/// 构建评估系统提示词
///
/// 三段式结构：角色定义 + 评估维度 + 用户评估要求
pub fn build_evaluation_system_prompt(eval_requirements: &[EvalRequirement]) -> String {
    let mut prompt = String::new();

    // ── 第一段：角色定义 ──
    prompt.push_str(r#"# 角色定义

你是 AI Brain 系统的质量审核员，负责在主脑产生输出后评估其质量和安全性。

## 核心能力
- **任务完成度审查**：判断主脑是否真正完成了用户要求的任务
- **安全风险识别**：检测代码中的危险操作和安全隐患
- **代码质量审查**：识别未完成实现、偷懒占位、潜在缺陷
- **事实正确性校验**：验证技术细节、API 用法是否准确
- **偏好合规检查**：确认输出遵守用户的偏好和禁忌

## 工作方式
- 你基于记忆脑提供的踩坑库、用户画像、自进化规则进行判断
- 每次主脑输出后自动触发评估
- 只报告确实存在的问题，宁可漏报不误报

"#);

    // ── 第二段：评估维度（固定不变） ──
    prompt.push_str(r#"# 评估维度

对每次主脑输出，按以下维度逐一检查：

1. **任务完成度** — 主脑是否完成了用户要求的所有内容？有无遗漏关键步骤或要求？
2. **高危操作检测** — 输出中是否包含危险操作？如：文件删除(rm -rf)、force push、覆盖写入、unsafe 代码、未备份的破坏性修改等。
3. **代码完整性** — 是否存在 TODO、FIXME、省略号(...)、未实现的占位符？代码是否可以直接运行？
4. **事实正确性** — 技术细节、API 用法、库版本、语法规则等是否正确？
5. **用户偏好遵守** — 是否违反用户的显性偏好、隐性偏好或禁忌事项？
6. **已知错误复现** — 是否重复了踩坑库中记录的错误模式？（注意：只有主脑犯了技术/逻辑错误才算踩坑复现，用户反复问/测试/闲聊不算）

## 判定原则
- **踩坑 = 主脑的技术/逻辑错误**（代码 bug、错误事实、用 unsafe 替代安全方案）
- **踩坑 ≠ 用户行为**（反复问、测试、闲聊确认）
- **高危操作必须报告**，无论最终是否实际执行
- 区分踩坑记录内容和当前实际行为：看主脑当前做了什么，而非记录里写了什么
- 只有确信有问题才报告

"#);

    // ── 第三段：用户评估要求（动态积累） ──
    if !eval_requirements.is_empty() {
        prompt.push_str("# 用户评估要求\n\n");
        prompt.push_str("以下是用户对评估的具体要求和纠正，请严格遵循：\n\n");
        for (i, req) in eval_requirements.iter().enumerate() {
            let _ = writeln!(prompt, "{}. {}", i + 1, req.content);
        }
        prompt.push_str("\n**用户评估要求优先级高于固定评估维度。**\n\n");
    }

    // ── 输出格式 ──
    prompt.push_str(r#"# 输出格式

没有问题时，严格输出（不要附加其他文字）：
评估结果-正常

有问题时，严格输出（不要附加其他文字）：
评估结果-存在问题。具体问题：1.问题描述及修正建议 2.问题描述及修正建议 ... 需要理解根据问题和要求/需求继续修改。

注意：不要输出 JSON，不要使用代码块，只输出纯文本。"#);

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

    prompt.push_str("请根据以上信息评估主脑输出。没有问题输出「评估结果-正常」，有问题输出「评估结果-存在问题。具体问题：...」");

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
    fn system_prompt_has_role_definition() {
        let prompt = build_evaluation_system_prompt(&[]);
        assert!(prompt.contains("角色定义"));
        assert!(prompt.contains("质量审核员"));
        assert!(prompt.contains("核心能力"));
    }

    #[test]
    fn system_prompt_has_fixed_dimensions() {
        let prompt = build_evaluation_system_prompt(&[]);
        assert!(prompt.contains("评估维度"));
        assert!(prompt.contains("任务完成度"));
        assert!(prompt.contains("高危操作"));
        assert!(prompt.contains("代码完整性"));
        assert!(prompt.contains("事实正确性"));
        assert!(prompt.contains("用户偏好遵守"));
        assert!(prompt.contains("已知错误复现"));
    }

    #[test]
    fn system_prompt_has_output_format() {
        let prompt = build_evaluation_system_prompt(&[]);
        assert!(prompt.contains("评估结果-正常"));
        assert!(prompt.contains("评估结果-存在问题"));
        assert!(prompt.contains("不要输出 JSON"));
    }

    #[test]
    fn system_prompt_has_judgment_principles() {
        let prompt = build_evaluation_system_prompt(&[]);
        assert!(prompt.contains("判定原则"));
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
        let prompt = build_evaluation_system_prompt(&reqs);
        assert!(prompt.contains("用户评估要求"));
        assert!(prompt.contains("不要将简单问答判定为问题"));
        assert!(prompt.contains("重点关注代码安全性"));
        assert!(prompt.contains("优先级高于固定评估维度"));
    }

    #[test]
    fn system_prompt_without_eval_requirements() {
        let prompt = build_evaluation_system_prompt(&[]);
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
            superseded: false,
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
