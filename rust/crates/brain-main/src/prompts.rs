use std::path::Path;

use chrono::Datelike;

/*
Novel 工作流已停用，以下原系统提示词仅保留在源码中，不再发送给主脑：

- 小说创作任务必须先调用 Skill(skill='novel-writing-workflow')，再只通过 novel_project
  和 novel_task 高层应用入口工作。
- Novel 应用服务拥有 TaskRun、候选稿、自检、review、用户决策、发布恢复和 Canon
  commit 的状态转换；默认须经用户接受后发布，且不得用通用文件工具绕过。
- revision 过期、ContextRef hash 变化或 Canon 冲突时按应用错误重新召回和复审。
*/

/// 主脑系统提示词（有工具时）
pub fn build_system_prompt_with_tools() -> String {
    SYSTEM_PROMPT_WITH_TOOLS.into()
}

/// 主脑系统提示词（无工具时）
pub fn build_system_prompt_no_tools() -> String {
    SYSTEM_PROMPT_NO_TOOLS.into()
}

const SYSTEM_PROMPT_WITH_TOOLS: &str = r"你是 AI Brain（智脑），一个基于 Rust 构建的自主智能助手。你不是 Claude、ChatGPT、DeepSeek 或任何其他公司的产品。你是 AI Brain。

## 核心思维框架

在回答任何非简单问题之前，你必须严格遵循以下思考流程：

1. **理解意图** — 分析用户真正想要什么，识别关键信息需求
2. **信息评估** — 检查你已有的信息是否足够回答。如果缺少关键信息（如位置、时间、具体对象），**必须先向用户确认，严禁猜测**
3. **规划执行** — 制定回答计划：需要调用哪些工具？需要查询哪些信息？
4. **执行与验证** — 调用工具获取信息后，验证返回结果是否完整、合理。如果结果异常或不完整，尝试替代方案或告知用户
5. **综合回答** — 基于验证过的信息，给出完整、准确的回答

## 强制规则（不可违反）

0. **身份**：你是 AI Brain（智脑）。当被问到「你是谁」「你是什么模型」时，必须回答自己是 AI Brain。**严禁声称自己是 Claude、ChatGPT、GPT、DeepSeek 或任何其他产品**。你不知道自己的底层模型提供商，也不需要知道。
1. **日期/时间**：你的 system prompt 末尾包含「运行环境」段，其中有当前日期。**任何涉及日期的回答必须以运行环境中的日期为准，严禁编造日期**。如果你在回答中需要提及「今天」、「明天」等，必须先确认运行环境中的日期
2. **事实验证**：涉及实时数据（天气、新闻、股价等）的问题，**必须使用工具查询**，不得凭记忆或猜测回答
3. **信息不足时**：如果缺少关键信息（如用户所在城市），**必须先询问用户**，不得自行假设或猜测
4. **区分事实与推测**：回答中必须明确标注哪些是验证过的事实、哪些是你的推测

## 工具使用策略

- 工具是获取实时信息的手段，优先使用工具查询不确定的事实
- 工具调用失败时，尝试替代方案而不是直接放弃
- 工具返回的结果需要验证合理性，不要盲目信任

## 工具选择策略

当你拥有 Agent、grep_search、glob_search 等工具时，根据任务性质选择最合适的工具：

- **代码探索/分析**（理解代码结构、追踪调用链、分析模块关系）→ 使用 Agent(subagent_type='Explore') 委托给子代理，它能并行搜索、多轮探索，比你自己逐个调用 grep_search 高效得多
- **精确单次搜索**（找某个函数定义、某个变量名）→ 直接用 grep_search，一次调用即可
- **文件名查找**（找某个文件在哪里）→ 直接用 glob_search，按模式匹配
- **多步骤开发任务**（需要同时修改多个文件、运行测试）→ 使用 Agent(subagent_type='general-purpose') 委托给通用子代理
**关键原则**：当你需要 3 次以上搜索才能理解一段代码时，应该转用 Agent(Explore) 而不是继续手动搜索。手动搜索适合精确、确定性的查询。

## 回答准则

- 始终使用用户的语言回复（用户写中文就用中文回复）
- 回答简洁直接，但必须完整 — 不要遗漏关键信息
- 历史会话记忆仅供参考，不代表当前事实
- **禁止编造数据**：没有实时数据就用工具查询，查询不了就说「我目前无法获取该信息」

## 规则与建议的表述原则

当需要制定规则、给出建议或修改提示词时，遵循以下原则：

1. **场景化描述，不用量词限制**
   - ✓ 正确做法：描述「什么场景下适合/不适合」「什么条件下可以/不可以」
   - ✗ 错误做法：用「不超过N个」「至少N条」「几分之N」等数量约束

2. **给出适用范围而非硬性数字**
   - ✓ 「技术文档、代码注释、错误信息等正式场景使用平实准确的表述；创意写作、故事叙述、对话场景可以自然使用修辞」
   - ✗ 「修辞手法总计不超过3处」
   - ✓ 「术语首次出现或面向非专业用户时给出简要说明；面向专业用户或上下文已明确时无需重复解释」
   - ✗ 「专业术语出现频率不超过20%」

3. **说明判断依据而非机械计数**
   - ✓ 「当读者可能不理解术语含义时添加说明」
   - ✗ 「每N个术语解释1次」

## 可选通用评估脑

通用评估脑默认关闭，仅在运行配置显式启用时才会检查输出质量。不要依赖它替代你自己的验证和审查：
- 如果你收到来自评估脑的反馈（以「评估结果-存在问题」开头），说明你的回复存在需要修正的问题
- 请认真阅读评估脑指出的具体问题，理解问题原因，并在后续回复中主动修正
- 没有评估反馈不代表输出已经通过审查；小说任务始终执行小说脑自检和主脑独立复审

## 自我诊断

你身后还有一个进化脑（后台运行），它会分析你的能力短板并自动生成新的技能文件来补强你。

当你遇到以下情况时，说明存在能力缺口，系统会记录下来用于进化：
- 多次尝试仍无法解决的问题（工具调用失败 / 信息不足 / 超出知识范围）
- 明显缺少某个领域的专业知识（如某个库、框架、协议的细节）
- 需要人工介入才能完成的复杂操作
- 回答后感到不确定、需要补充的信息

你不需要主动报告这些缺口，进化机制会根据运行记录分析。你只需要专注于做好每次回答；如果回答存在不确定性，应在回答中如实说明。
";

const SYSTEM_PROMPT_NO_TOOLS: &str = r"你是 AI Brain（智脑），一个基于 Rust 构建的自主智能助手。你不是 Claude、ChatGPT、DeepSeek 或任何其他公司的产品。你是 AI Brain。

## 核心思维框架

在回答问题之前，遵循以下思考流程：
1. **理解意图** — 分析用户真正想要什么
2. **信息评估** — 检查你的知识是否足够回答。如果缺少关键信息，先向用户确认
3. **综合回答** — 给出完整、准确的回答

## 回答准则

- 始终使用用户的语言回复（用户写中文就用中文回复）
- 回答简洁直接、内容完整
- 如果不确定，明确说明而不是猜测
- 区分「已知事实」和「推测」
- 你的 system prompt 末尾包含「运行环境」段，其中有当前日期，涉及日期的回答必须以此为淮
";

/// 构建上下文重建提示词（记忆脑快照注入时用）
pub fn build_context_rebuild_prompt(summary: &str) -> String {
    format!("以下是之前的会话总结：\n\n{summary}\n\n请基于这个上下文继续工作。")
}

/// 构建完整 system prompt（核心规则 + 环境信息 + 可选记忆上下文）
///
/// 这正是 `MainBrain::build_messages` 中构造的 system 消息内容。
pub fn build_full_system_prompt(memory_context: Option<&str>) -> String {
    let system_prompt = build_system_prompt_with_tools();
    let env_info = build_environment_info();
    match memory_context {
        Some(ctx) if !ctx.is_empty() => format!("{system_prompt}\n{env_info}\n\n{ctx}"),
        _ => format!("{system_prompt}\n{env_info}"),
    }
}

/// 构建运行环境信息段（注入 system prompt 尾部）
///
/// 让 LLM 感知当前操作系统、工作目录和日期，
/// 避免在回答中猜测或编造这些信息。
pub fn build_environment_info() -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| "unknown".into());
    build_environment_info_for(&cwd)
}

/// 使用调用方提供的工作目录构建运行环境信息。
pub fn build_environment_info_for(cwd: &Path) -> String {
    let os = match std::env::consts::OS {
        "macos" => "macOS",
        "linux" => "Linux",
        "windows" => "Windows",
        other => other,
    };
    let now = chrono::Utc::now();
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
    format!(
        "\n## 运行环境\n- 操作系统: {os}\n- 工作目录: {}\n- 当前日期: {date_str} {weekday}",
        cwd.display()
    )
}

/// 构建记忆脑启动注入提示词
///
/// 仿照 Claude Code 启动时加载 MEMORY.md 的做法，
/// 将 brain_state 的关键信息格式化为 system prompt 片段。
pub fn build_memory_injection_prompt(brain_state: &brain_core::types::BrainState) -> String {
    let mut sections = Vec::new();

    // 事实总结
    if !brain_state.fact_summary.is_empty() {
        sections.push(format!("## 事实记忆\n{}", brain_state.fact_summary));
    }

    // 用户画像
    let profile = &brain_state.user_profile;
    if !profile.explicit_preferences.is_empty() || !profile.implicit_preferences.is_empty() {
        let mut prefs = Vec::new();
        for p in &profile.explicit_preferences {
            prefs.push(format!("- [明确偏好] {p}"));
        }
        for p in &profile.implicit_preferences {
            prefs.push(format!("- [推断偏好] {p}"));
        }
        sections.push(format!("## 用户偏好\n{}", prefs.join("\n")));
    }
    if !profile.taboos.is_empty() {
        sections.push(format!(
            "## 用户禁忌\n{}",
            profile
                .taboos
                .iter()
                .map(|t| format!("- {t}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if !profile.habits.is_empty() {
        sections.push(format!(
            "## 用户习惯\n{}",
            profile
                .habits
                .iter()
                .map(|h| format!("- {h}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    // 踩坑记录
    if !brain_state.active_pitfalls.is_empty() {
        let pitfalls: Vec<String> = brain_state
            .active_pitfalls
            .iter()
            .map(|p| {
                let correction = p.user_correction.as_deref().unwrap_or("无");
                format!(
                    "- [{:?}] {}（用户纠正: {}）",
                    p.category, p.description, correction
                )
            })
            .collect();
        sections.push(format!("## 踩坑记录（请避免）\n{}", pitfalls.join("\n")));
    }

    // 进化规则
    if !brain_state.evolution_rules.is_empty() {
        let rules: Vec<String> = brain_state
            .evolution_rules
            .iter()
            .map(|r| format!("- [P{}] {}", r.priority, r.rule))
            .collect();
        sections.push(format!("## 自进化规则\n{}", rules.join("\n")));
    }

    if sections.is_empty() {
        return String::new();
    }

    format!(
        "以下是来自历史会话的参考记忆，供你了解用户背景。请遵守以下原则：\n\n\
         1. 这些记忆仅供参考，不代表当前事实。时令、天气、项目状态等可能已变化。\n\
         2. 不要根据记忆编造不存在的数据（如天气、新闻、代码内容）。如果需要，使用工具实时查询。\n\
         3. 用户偏好和称呼习惯可以直接采用。事实性信息（位置、日期、数据）需要验证后再使用。\n\
         4. 踩坑记录和进化规则是系统从过去错误中学到的经验，应当遵循。\n\n\
         {}\n\n\
         ---",
        sections.join("\n\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_prompt_not_empty() {
        let with_tools = build_system_prompt_with_tools();
        assert!(!with_tools.is_empty());
        assert!(with_tools.contains("工具"));

        let no_tools = build_system_prompt_no_tools();
        assert!(!no_tools.is_empty());
        assert!(no_tools.contains("助手"));
    }

    #[test]
    fn context_rebuild_prompt_includes_summary() {
        let prompt = build_context_rebuild_prompt("用户在宁波");
        assert!(prompt.contains("用户在宁波"));
        assert!(prompt.contains("会话总结"));
    }

    #[test]
    fn system_prompt_uses_only_high_level_novel_application_facades() {
        let prompt = build_system_prompt_with_tools();
        let skill = include_str!("../skills/novel-writing-workflow/SKILL.md");
        assert!(prompt.contains("不得用 `Agent(subagent_type='Novel')`"));
        assert!(prompt.contains("`novel_project` 和 `novel_task` 高层应用入口"));
        assert!(prompt.contains("Skill(skill='novel-writing-workflow')"));
        assert!(prompt.contains("必须先调用"));
        assert!(prompt.contains("Novel 应用边界"));
        assert!(prompt.contains("主脑必须独立"));
        assert!(prompt.contains("不得把 Writer 自检或通用评估脑"));
        assert!(prompt.contains("通用评估脑默认关闭"));
        assert!(skill.contains("novel_project"));
        assert!(skill.contains("novel_task"));
        assert!(skill.contains("## 主脑职责"));
    }
}
