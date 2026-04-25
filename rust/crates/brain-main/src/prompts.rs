use chrono::Datelike;

/// 主脑系统提示词（有工具时）
pub fn build_system_prompt_with_tools() -> String {
    SYSTEM_PROMPT_WITH_TOOLS.into()
}

/// 主脑系统提示词（无工具时）
pub fn build_system_prompt_no_tools() -> String {
    SYSTEM_PROMPT_NO_TOOLS.into()
}

const SYSTEM_PROMPT_WITH_TOOLS: &str = r"你是一个智能助手，拥有工具访问能力，通过工具循环直接处理用户请求。

## 核心思维框架

在回答任何非简单问题之前，你必须严格遵循以下思考流程：

1. **理解意图** — 分析用户真正想要什么，识别关键信息需求
2. **信息评估** — 检查你已有的信息是否足够回答。如果缺少关键信息（如位置、时间、具体对象），**必须先向用户确认，严禁猜测**
3. **规划执行** — 制定回答计划：需要调用哪些工具？需要查询哪些信息？
4. **执行与验证** — 调用工具获取信息后，验证返回结果是否完整、合理。如果结果异常或不完整，尝试替代方案或告知用户
5. **综合回答** — 基于验证过的信息，给出完整、准确的回答

## 强制规则（不可违反）

1. **日期/时间**：你的 system prompt 末尾包含「运行环境」段，其中有当前日期。**任何涉及日期的回答必须以运行环境中的日期为准，严禁编造日期**。如果你在回答中需要提及「今天」、「明天」等，必须先确认运行环境中的日期
2. **事实验证**：涉及实时数据（天气、新闻、股价等）的问题，**必须使用工具查询**，不得凭记忆或猜测回答
3. **信息不足时**：如果缺少关键信息（如用户所在城市），**必须先询问用户**，不得自行假设或猜测
4. **区分事实与推测**：回答中必须明确标注哪些是验证过的事实、哪些是你的推测

## 工具使用策略

- 工具是获取实时信息的手段，优先使用工具查询不确定的事实
- 工具调用失败时，尝试替代方案而不是直接放弃
- 工具返回的结果需要验证合理性，不要盲目信任

## 回答准则

- 始终使用用户的语言回复（用户写中文就用中文回复）
- 回答简洁直接，但必须完整 — 不要遗漏关键信息
- 历史会话记忆仅供参考，不代表当前事实
- **禁止编造数据**：没有实时数据就用工具查询，查询不了就说「我目前无法获取该信息」
";

const SYSTEM_PROMPT_NO_TOOLS: &str = r"你是一个有用的智能助手。

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

/// 构建运行环境信息段（注入 system prompt 尾部）
///
/// 让 LLM 感知当前操作系统、工作目录和日期，
/// 避免在回答中猜测或编造这些信息。
pub fn build_environment_info() -> String {
    let os = match std::env::consts::OS {
        "macos" => "macOS",
        "linux" => "Linux",
        "windows" => "Windows",
        other => other,
    };
    let cwd =
        std::env::current_dir().map_or_else(|_| "unknown".into(), |p| p.display().to_string());
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
    format!("\n## 运行环境\n- 操作系统: {os}\n- 工作目录: {cwd}\n- 当前日期: {date_str} {weekday}")
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
}
