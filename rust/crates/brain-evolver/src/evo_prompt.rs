//! EvoPrompt — 进化脑专用 System Prompt 生成器
//!
//! 五层结构:
//! - Layer 1: 身份定义（你是谁）
//! - Layer 2: 进化方法论（六阶段循环）
//! - Layer 3: 知识上下文（能力树 + 已有 skills + backlog）
//! - Layer 4: 质量标准（SKILL.md 要求）
//! - Layer 5: 运行环境（时间 + 工具 + 预算）

/// Context for building the evolution system prompt.
#[derive(Clone, Debug, Default)]
pub struct EvoPromptContext {
    /// Description of the current evolution target.
    pub current_target: String,
    /// Summary of the capability tree.
    pub capability_tree_summary: String,
    /// Names of already registered skills.
    pub existing_skill_names: Vec<String>,
    /// Related backlog entries.
    pub related_backlog_entries: Vec<String>,
    /// Notes from the previous interrupted cycle (cross-night resume).
    pub previous_cycle_notes: Option<String>,
    /// List of available tools (MCP tools).
    pub tool_list: Vec<String>,
    /// Token budget for this target.
    pub token_budget: u64,
}

/// Build the complete evolution system prompt.
pub fn build_evo_system_prompt(ctx: &EvoPromptContext) -> String {
    let mut prompt = String::new();

    // Layer 1: 身份定义
    prompt.push_str(&build_identity_layer(&ctx.current_target));
    prompt.push_str("\n\n");

    // Layer 2: 进化方法论
    prompt.push_str(build_methodology_layer());
    prompt.push_str("\n\n");

    // Layer 3: 知识上下文
    prompt.push_str(&build_context_layer(ctx));
    prompt.push_str("\n\n");

    // Layer 4: 质量标准
    prompt.push_str(build_quality_layer());
    prompt.push_str("\n\n");

    // Layer 5: 运行环境
    prompt.push_str(&build_environment_layer(ctx));

    prompt
}

fn build_identity_layer(target: &str) -> String {
    format!(
        "# 身份定义\n\n\
         你是智脑的进化子系统，在夜间独立运行。\n\
         你拥有主脑的全部能力：推理、工具调用、记忆、评估。\n\
         你的使命：持续提升智脑的能力边界。\n\
         你当前正在处理的目标：{target}"
    )
}

fn build_methodology_layer() -> &'static str {
    "# 进化方法论\n\n\
     六阶段循环，每个阶段有明确的完成标准：\n\n\
     1. **感知 (Perceive)**: 分析目标、识别知识缺口、召回上次进度\n\
     2. **研究 (Research)**: 联网搜索一手资料，收集有价值的原始信息\n\
     3. **学习 (Learn)**: 对话式消化知识，四步分析（事实→模式→经验→触发词）\n\
     4. **合成 (Synthesize)**: 整理为结构化 SKILL.md，包含触发条件+知识正文+示例\n\
     5. **注册 (Register)**: 写入技能目录，自动注册到 SkillCatalog\n\
     6. **验证 (Verify)**: 独立验证代理审核质量，不达标则回到学习阶段\n\n\
     迭代上限 3 次，超过则报告阻塞。\n\
     研究优先用 MCP 工具联网获取一手资料。"
}

fn build_context_layer(ctx: &EvoPromptContext) -> String {
    let mut layer = String::from("# 知识上下文\n\n");

    if !ctx.capability_tree_summary.is_empty() {
        layer.push_str(&format!(
            "## 当前能力树\n{}\n\n",
            ctx.capability_tree_summary
        ));
    }

    if !ctx.existing_skill_names.is_empty() {
        layer.push_str(&format!(
            "## 已有技能\n{}\n\n",
            ctx.existing_skill_names.join(", ")
        ));
    }

    if !ctx.related_backlog_entries.is_empty() {
        layer.push_str("## 相关积压问题\n");
        for entry in &ctx.related_backlog_entries {
            layer.push_str(&format!("- {entry}\n"));
        }
        layer.push('\n');
    }

    if let Some(notes) = &ctx.previous_cycle_notes {
        layer.push_str(&format!("## 上次进化积累\n{notes}\n\n"));
    }

    layer
}

fn build_quality_layer() -> &'static str {
    "# 质量标准\n\n\
     SKILL.md 必须包含：\n\
     - **触发条件**: 什么情况下使用此技能\n\
     - **知识正文**: 核心知识点、原理、模式\n\
     - **示例**: 实际代码示例或应用场景\n\
     - **引用来源**: 知识的可验证出处\n\n\
     约束：\n\
     - 不允许模糊的概括，每个结论必须有具体依据\n\
     - 与已有 skill 矛盾的知识必须明确标注差异并说明\n\
     - 知识必须来自可验证的来源（标注引用）"
}

fn build_environment_layer(ctx: &EvoPromptContext) -> String {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC");
    let mut layer = format!("# 运行环境\n\n- 当前时间: {now}\n");

    if !ctx.tool_list.is_empty() {
        layer.push_str("- 可用工具: ");
        layer.push_str(&ctx.tool_list.join(", "));
        layer.push('\n');
    }

    layer.push_str(&format!(
        "- 资源预算: 单目标最多 {} tokens\n",
        ctx.token_budget
    ));

    layer
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_context() -> EvoPromptContext {
        EvoPromptContext {
            current_target: "Rust async 编程体系".into(),
            capability_tree_summary: "Rust: 基础✓ 错误处理✓ async✗".into(),
            existing_skill_names: vec!["rust-basics".into(), "rust-error-handling".into()],
            related_backlog_entries: vec!["async runtime 模型理解不足".into()],
            previous_cycle_notes: Some("已掌握 Pin 语义，缺 Send 约束".into()),
            tool_list: vec!["web_search".into(), "web_reader".into()],
            token_budget: 100_000,
        }
    }

    #[test]
    fn test_prompt_contains_target() {
        let ctx = make_context();
        let prompt = build_evo_system_prompt(&ctx);
        assert!(prompt.contains("Rust async 编程体系"));
    }

    #[test]
    fn test_prompt_contains_skills() {
        let ctx = make_context();
        let prompt = build_evo_system_prompt(&ctx);
        assert!(prompt.contains("rust-basics"));
        assert!(prompt.contains("rust-error-handling"));
    }

    #[test]
    fn test_prompt_contains_methodology() {
        let ctx = make_context();
        let prompt = build_evo_system_prompt(&ctx);
        assert!(prompt.contains("感知 (Perceive)"));
        assert!(prompt.contains("验证 (Verify)"));
        assert!(prompt.contains("迭代上限 3 次"));
    }

    #[test]
    fn test_prompt_contains_quality_standards() {
        let ctx = make_context();
        let prompt = build_evo_system_prompt(&ctx);
        assert!(prompt.contains("触发条件"));
        assert!(prompt.contains("引用来源"));
    }

    #[test]
    fn test_prompt_contains_environment() {
        let ctx = make_context();
        let prompt = build_evo_system_prompt(&ctx);
        assert!(prompt.contains("100000 tokens"));
        assert!(prompt.contains("web_search"));
    }

    #[test]
    fn test_prompt_with_minimal_context() {
        let ctx = EvoPromptContext {
            current_target: "test".into(),
            ..Default::default()
        };
        let prompt = build_evo_system_prompt(&ctx);
        assert!(prompt.contains("test"));
        assert!(prompt.contains("进化方法论"));
    }

    #[test]
    fn test_prompt_with_previous_notes() {
        let ctx = EvoPromptContext {
            current_target: "test".into(),
            previous_cycle_notes: Some("上次学到了 X".into()),
            ..Default::default()
        };
        let prompt = build_evo_system_prompt(&ctx);
        assert!(prompt.contains("上次学到了 X"));
    }

    #[test]
    fn test_prompt_different_targets() {
        let ctx1 = EvoPromptContext {
            current_target: "target-A".into(),
            ..Default::default()
        };
        let ctx2 = EvoPromptContext {
            current_target: "target-B".into(),
            ..Default::default()
        };
        let p1 = build_evo_system_prompt(&ctx1);
        let p2 = build_evo_system_prompt(&ctx2);
        assert_ne!(p1, p2);
        assert!(p1.contains("target-A"));
        assert!(p2.contains("target-B"));
    }
}
