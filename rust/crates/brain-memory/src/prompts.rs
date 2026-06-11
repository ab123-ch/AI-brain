//! System prompts for memory brain operations.
//!
//! 包含：
//! - L1→L2、L2→L3 巩固 prompt
//! - v2 四步分析 prompt（事实总结 / 用户画像 / 踩坑分析 / 自进化规则）

// ---------------------------------------------------------------------------
// L1→L2 Index Consolidation Prompt
// ---------------------------------------------------------------------------

/// Prompt for generating L2 index entries from L1 raw records.
pub const L1_TO_L2_PROMPT: &str = "\
你是一个记忆索引专家。请阅读以下原始记忆记录，为每条记录生成索引摘要。

要求：
1. **分类**：根据内容判断属于哪个类别（debugging/development/architecture/testing/deployment/general）
2. **摘要**：用 2-3 句话高度概括核心内容，保留所有关键信息（技术细节、具体数值、文件路径等不能丢失）
3. **标签**：提取 3-5 个关键词标签
4. **重要性**：评估 0.0-1.0 的重要性分数
5. **分组**：内容相关的记录应合并为一条索引

原始记录：
{raw_entries_json}

请严格按以下 JSON 格式返回（数组）：
[
  {
    \"category\": \"debugging\",
    \"summary\": \"高度概括的摘要内容...\",
    \"tags\": [\"关键词1\", \"关键词2\"],
    \"importance\": 0.8,
    \"grouped_entry_ids\": [\"raw-xxx\", \"raw-yyy\"]
  }
]\
";

// ---------------------------------------------------------------------------
// L2→L3 Experience Pack Consolidation Prompt
// ---------------------------------------------------------------------------

/// Prompt for generating L3 experience packs from L2 index entries.
pub const L2_TO_L3_PROMPT: &str = "\
你是一个经验提炼专家。请阅读以下同一类别的索引摘要，提炼出可复用的经验包。

要求：
1. **标题**：用一句话概括这组经验
2. **触发模式**：什么场景下应该参考这个经验（2-3 个模式描述）
3. **推理路径**：总结出最佳实践的步骤
4. **错误教训**：识别犯过的错误及避免方法
5. **经验捷径**：提炼出 1-3 条简洁的经验总结
6. **上下文片段**：生成一段可以直接注入推理脑的精炼文本（200字以内），帮助推理脑避免重复犯错和重复探索

类别：{category}
索引摘要：
{index_entries_json}

请严格按以下 JSON 格式返回：
{
  \"title\": \"经验包标题\",
  \"trigger_patterns\": [\"模式1\", \"模式2\"],
  \"reasoning_path\": [\"步骤1\", \"步骤2\", \"步骤3\"],
  \"mistakes\": [{\"what\": \"错误描述\", \"why\": \"原因\", \"how_to_avoid\": \"避免方法\"}],
  \"shortcuts\": [\"经验总结1\", \"经验总结2\"],
  \"context_snippet\": \"可直接注入推理脑的精炼经验文本\",
  \"should_retain\": true
}\
";

// ---------------------------------------------------------------------------
// v2 四步分析 Prompt
// ---------------------------------------------------------------------------

/// Step0: 记忆迭代分类（插入在 Step1 之前）
pub const STEP0_MEMORY_ITERATION_PROMPT: &str = "\
# 身份
你是一个记忆迭代分类引擎。你分析新会话的事实总结，判断与已有记忆之间的关系类型。

# 输入
新会话事实总结：
{new_fact_summary}

已有记忆条目：
{existing_entries_json}

# 规则
1. 对每条已有记忆，判断它与新事实之间的关系：
   - OVERRIDE: 同一事实的不同结论（互斥，新结论取代旧结论）
   - COMPLEMENT: 同一话题的不同方面（互补，两边都保留）
   - REFINE: 新的是旧的细化/深化版本（旧标记 superseded）
   - UNRELATED: 完全无关（跳过）
2. 只分类关系，不判断事实对错
3. 如果没有匹配的已有记忆，返回空数组

# 输出格式（严格 JSON 数组）
[
  {\"id\": \"已有记忆的id\", \"relation\": \"OVERRIDE\"},
  {\"id\": \"另一个id\", \"relation\": \"COMPLEMENT\"}
]\
";

/// 构建 Step0 记忆迭代 prompt
pub fn build_step0_prompt(new_fact_summary: &str, existing_entries_json: &str) -> String {
    STEP0_MEMORY_ITERATION_PROMPT
        .replace("{new_fact_summary}", new_fact_summary)
        .replace("{existing_entries_json}", existing_entries_json)
}

/// 第一步：事实总结（增量模式）
///
/// 首轮：从对话中提取事实摘要
/// 后续轮：在已有摘要基础上，根据新对话内容更新/补充
pub const STEP1_FACT_SUMMARY_PROMPT: &str = "\
# 身份
你是一个对话分析引擎。你的任务是维护一份持续累积的事实摘要。

# 输入
{previous_summary_section}
以下是最新的对话记录（JSON 数组）：
{conversation_json}

# 规则
1. **增量更新**：在已有摘要的基础上，根据新对话内容进行更新和补充
2. 只总结客观事实：用户做了什么、讨论了什么、决定了什么
3. 保留所有技术细节：文件路径、命令、配置值、错误信息
4. 按时间顺序组织，形成连贯的叙事
5. 已有摘要中未涉及的内容保持不变，不要删除
6. 新对话中有新的进展、决定、发现时，追加到摘要中
7. 用中文输出
8. 摘要长度控制在 500 字以内（累积增长，但需要精炼）

# 输出格式
直接输出完整的事实摘要（纯文本，不要 JSON 包裹）。输出是更新后的完整摘要，不是增量部分。\
";

/// 第二步：用户画像分析
pub const STEP2_USER_PROFILE_PROMPT: &str = "\
# 身份
你是一个用户画像分析引擎。你分析对话记录，提取用户的显性偏好、隐性偏好、禁忌和工作习惯。

# 输入
对话记录摘要：
{fact_summary}

完整对话记录：
{conversation_json}

# 规则
1. **显性偏好**：用户明确说过的喜好（如[我喜欢简洁的代码]、[用 Rust]）
2. **隐性偏好**：从行为推断的偏好（如用户总是先写测试 -> 偏好 TDD）
3. **禁忌**：用户明确禁止或反感的事物（如[不要用 unwrap]、[别用 GC 语言]）
4. **习惯**：用户的工作模式（如[总是先读 README]、[喜欢分步骤执行]）
5. 每类最多提取 10 条
6. 不要重复已有的条目
7. 用中文输出

# 已有画像（去重用）
显性偏好：{existing_explicit}
隐性偏好：{existing_implicit}
禁忌：{existing_taboos}
习惯：{existing_habits}

# 输出格式（严格 JSON）
{
  \"explicit_preferences\": [\"偏好1\", \"偏好2\"],
  \"implicit_preferences\": [\"推断1\", \"推断2\"],
  \"taboos\": [\"禁忌1\"],
  \"habits\": [\"习惯1\"]
}\
";

/// 第三步：踩坑分析
pub const STEP3_PITFALL_PROMPT: &str = "\
# 身份
你是一个错误分析引擎。你分析对话记录，识别系统犯过的错误和可以改进的地方。

# 输入
对话记录摘要：
{fact_summary}

完整对话记录：
{conversation_json}

# 规则
1. 识别以下类型的错误：
   - **ToolFailure**: 工具调用失败（超时、参数错误、权限不足等）
   - **WrongAnswer**: 给出了错误或不完整的回答
   - **FormatIssue**: 输出格式不符合用户预期
   - **LazyBehavior**: 偷懒行为（如只读部分文件、跳过验证、猜测而非确认）
   - **Other**: 其他问题
2. 每条记录要具体：描述发生了什么、用户如何纠正
3. 不要重复已有的踩坑记录
4. 只记录实际发生的问题，不要推测
5. 用中文输出

# 已有踩坑记录（去重用）
{existing_pitfalls}

# 输出格式（严格 JSON）
{
  \"pitfalls\": [
    {
      \"category\": \"ToolFailure\",
      \"description\": \"具体描述\",
      \"user_correction\": \"用户如何纠正（没有则为 null）\"
    }
  ]
}\
";

/// 第四步：自进化规则提炼
pub const STEP4_EVOLUTION_PROMPT: &str = "\
# 身份
你是一个规则提炼引擎。你从踩坑记录中提炼出可执行的自进化规则，帮助系统在未来避免犯同样的错误。

# 输入
踩坑记录：
{pitfalls_json}

已有规则（去重用）：
{existing_rules}

# 规则
1. 每条规则必须来自具体的踩坑记录（标注来源 pitfall ID）
2. 规则必须可执行：明确告诉系统遇到什么情况应该做什么
3. 规则格式：\"当 [条件] 时，[行为]\"
4. 优先级 1-5（5 最高）：
   - 5: 必须遵守（会导致严重错误）
   - 4: 强烈建议（影响输出质量）
   - 3: 推荐遵守（提升效率）
   - 2: 可选优化
   - 1: 低优先级改进
5. 不要重复已有规则
6. 用中文输出

# 输出格式（严格 JSON）
{
  \"rules\": [
    {
      \"rule\": \"当 [条件] 时，[行为]\",
      \"source_pitfall_ids\": [\"pit-xxx\"],
      \"priority\": 4
    }
  ]
}\
";

// ---------------------------------------------------------------------------
// Prompt builder helpers
// ---------------------------------------------------------------------------

/// 构建 L1→L2 巩固 prompt
pub fn build_l1_to_l2_prompt(raw_entries_json: &str) -> String {
    L1_TO_L2_PROMPT.replace("{raw_entries_json}", raw_entries_json)
}

/// 构建 L2→L3 巩固 prompt
pub fn build_l2_to_l3_prompt(category: &str, index_entries_json: &str) -> String {
    L2_TO_L3_PROMPT
        .replace("{category}", category)
        .replace("{index_entries_json}", index_entries_json)
}

// ---------------------------------------------------------------------------
// v2 四步分析 Prompt Builder
// ---------------------------------------------------------------------------

/// 构建第一步：事实总结 prompt（增量模式）
pub fn build_step1_prompt(conversation_json: &str, previous_summary: Option<&str>) -> String {
    let previous_section = match previous_summary {
        Some(summary) => format!("## 已有的事实摘要（需要在此基础上更新）\n{summary}\n"),
        None => "（这是首次总结，没有已有摘要）\n".to_string(),
    };
    STEP1_FACT_SUMMARY_PROMPT
        .replace("{previous_summary_section}", &previous_section)
        .replace("{conversation_json}", conversation_json)
}

/// 构建第二步：用户画像分析 prompt
pub fn build_step2_prompt(
    fact_summary: &str,
    conversation_json: &str,
    existing_explicit: &str,
    existing_implicit: &str,
    existing_taboos: &str,
    existing_habits: &str,
) -> String {
    STEP2_USER_PROFILE_PROMPT
        .replace("{fact_summary}", fact_summary)
        .replace("{conversation_json}", conversation_json)
        .replace("{existing_explicit}", existing_explicit)
        .replace("{existing_implicit}", existing_implicit)
        .replace("{existing_taboos}", existing_taboos)
        .replace("{existing_habits}", existing_habits)
}

/// 构建第三步：踩坑分析 prompt
pub fn build_step3_prompt(
    fact_summary: &str,
    conversation_json: &str,
    existing_pitfalls: &str,
) -> String {
    STEP3_PITFALL_PROMPT
        .replace("{fact_summary}", fact_summary)
        .replace("{conversation_json}", conversation_json)
        .replace("{existing_pitfalls}", existing_pitfalls)
}

/// 构建第四步：自进化规则提炼 prompt
pub fn build_step4_prompt(pitfalls_json: &str, existing_rules: &str) -> String {
    STEP4_EVOLUTION_PROMPT
        .replace("{pitfalls_json}", pitfalls_json)
        .replace("{existing_rules}", existing_rules)
}

/// 构建第五步：潜意识叙事更新 prompt
pub fn build_step5_prompt(
    fact_summary: &str,
    pitfalls_text: &str,
    evolution_text: &str,
    existing_narrative: &str,
) -> String {
    STEP5_SUBCONSCIOUS_PROMPT
        .replace("{fact_summary}", fact_summary)
        .replace("{pitfalls_text}", pitfalls_text)
        .replace("{evolution_text}", evolution_text)
        .replace("{existing_narrative}", existing_narrative)
}

/// 第五步：潜意识叙事更新
///
/// 从结构化条目改为流动叙事，LLM 负责 合并/覆盖/追加 决策。
pub const STEP5_SUBCONSCIOUS_PROMPT: &str = "\
# 身份
你是一个记忆叙事编辑器。你负责维护一段关于用户的流动叙事文本——\"用户做过什么、踩过什么坑、比较过什么\"。
你的目标是：用最少的文字覆盖最多的经验触发面。

# 输入
本轮事实总结：
{fact_summary}

本轮新增踩坑：
{pitfalls_text}

本轮新增进化规则：
{evolution_text}

已有叙事文本：
{existing_narrative}

# 准入门槛（关键！不是所有对话都值得更新叙事）
只有满足以下至少一项才能更新叙事：
✅ 踩坑/故障排查经验
✅ 架构决策或技术选型
✅ 用户明确的偏好/禁忌/工作习惯
✅ 跨项目可复用的经验教训
✅ 复杂业务逻辑的关键理解

以下内容【禁止】更新叙事：
❌ 一次性闲聊、寒暄
❌ 通用编程知识
❌ 浅层问答
❌ 自我介绍/功能说明

# 叙事编辑规则（关键！）
1. **合并**：相关领域的经验自然融合为一句（如\"开发过AI Brain记忆脑（四步分析+三层存储+迭代机制）\"）
2. **覆盖**：新信息推翻旧结论时，自然替换，不保留被推翻的内容
3. **追加**：全新领域追加到叙事末尾
4. **控制篇幅**：叙事总长不超过 300 字，用逗号/顿号连接短语
5. **关键词**：提取能触发召回的关键词（最多 15 个），不要重复已有叙事中的每个词
6. 如果本轮对话没有产生有价值的新经验，返回空 narrative
7. 用中文输出

# 输出格式（严格 JSON）
{
  \"narrative\": \"做过X，做过Y，踩过Z的坑\",
  \"new_keywords\": [\"关键词1\", \"关键词2\"]
}

无新经验时：
{
  \"narrative\": \"\",
  \"new_keywords\": []
}\
";

// ---------------------------------------------------------------------------
// Step6: L2 会话总结 Prompt
// ---------------------------------------------------------------------------

/// 第六步：生成 L2 会话总结
pub const STEP6_SESSION_SUMMARY_PROMPT: &str = "\
# 身份
你是一个会话总结引擎。你将一次对话的事实总结、踩坑记录和决策提炼为精炼的会话总结。

# 输入
事实总结：
{fact_summary}

本轮踩坑记录：
{pitfalls_text}

已有潜意识叙事（参考用）：
{existing_subconscious}

# 规则
1. fact_summary：用 2-3 句话概括本次会话做了什么（200 字以内）
2. tags：提取 3-8 个关键词标签（中文为主，英文术语保留）
3. pitfalls：列出遇到的踩坑（每条 50 字以内，没有则空数组）
4. decisions：列出达成的决策或关键结论（每条 50 字以内，没有则空数组）
5. 用中文输出

# 输出格式（严格 JSON）
{
  \"fact_summary\": \"讨论了记忆脑层级重构，决定 L2 改总结层、L1 改归档层\",
  \"tags\": [\"记忆脑\", \"层级重构\", \"四步分析\"],
  \"pitfalls\": [\"旧 L2 存原文导致关键词噪音大\"],
  \"decisions\": [\"L2 由四步分析 Step6 生成\", \"L1 归档等守护线程实现\"]
}\
";

/// 构建第六步：会话总结 prompt
pub fn build_step6_prompt(
    fact_summary: &str,
    pitfalls_text: &str,
    existing_subconscious: &str,
) -> String {
    STEP6_SESSION_SUMMARY_PROMPT
        .replace("{fact_summary}", fact_summary)
        .replace("{pitfalls_text}", pitfalls_text)
        .replace("{existing_subconscious}", existing_subconscious)
}

/// 构建第七步：用户评估要求提取 prompt
pub fn build_step7_prompt(
    fact_summary: &str,
    conversation_json: &str,
    existing_requirements: &str,
) -> String {
    STEP7_EVAL_REQUIREMENTS_PROMPT
        .replace("{fact_summary}", fact_summary)
        .replace("{conversation_json}", conversation_json)
        .replace("{existing_requirements}", existing_requirements)
}

/// 第七步：用户评估要求提取
///
/// 从对话中识别用户对评估脑输出质量的反馈、纠正和偏好，
/// 转化为结构化的评估要求，持续优化评估脑的行为。
pub const STEP7_EVAL_REQUIREMENTS_PROMPT: &str = "\
# 身份
你是一个评估要求提取引擎。你的任务是从对话中识别用户对评估脑（质量审核员）行为的反馈和要求。

# 输入
事实总结：
{fact_summary}

对话记录：
{conversation_json}

已有评估要求（去重用）：
{existing_requirements}

# 提取目标
从对话中提取以下类型的用户评估要求：

1. **评估纠正** — 用户对评估脑的判定提出异议
   - 例：「评估脑把简单问答判为问题了」「这个不该报错」
   - 转化为：「简单问答和闲聊内容不应判定为问题」

2. **评估维度补充** — 用户希望评估脑关注新维度
   - 例：「希望你也检查代码的安全性」「注意有没有内存泄漏」
   - 转化为：「检查代码中是否存在安全隐患（内存泄漏、SQL 注入等）」

3. **评估方式调整** — 用户对评估的严格程度、表达方式有要求
   - 例：「评估太严格了」「不要那么敏感」
   - 转化为：「降低评估敏感度，只报告确定的高危问题」

4. **任务质量要求** — 用户对主脑任务完成的标准
   - 例：「回答必须包含代码示例」「代码必须能直接编译运行」
   - 转化为：「主脑输出的代码必须完整可运行，不能有占位符」

# 规则
- 只提取**明确**的用户反馈，不要推测
- 每条要求应该是**具体可执行**的指导规则
- 如果对话中没有与评估相关的反馈，返回空数组
- 不要重复已有评估要求中的内容
- content 字段用简洁的陈述句，描述评估脑应该怎么做

# 输出格式（严格 JSON）
```json
{
  \"requirements\": [
    {
      \"content\": \"具体的评估要求描述\",
      \"source\": \"用户反馈|记忆脑分析\"
    }
  ]
}
```

# 示例
对话中有：「你评估太敏感了，简单的打招呼不要判成问题」
输出：
```json
{
  \"requirements\": [
    {
      \"content\": \"简单打招呼、确认回复不应判定为问题，跳过此类内容的评估\",
      \"source\": \"用户反馈\"
    }
  ]
}
```\
";

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_not_empty() {
        assert!(!L1_TO_L2_PROMPT.is_empty());
        assert!(!L2_TO_L3_PROMPT.is_empty());
        assert!(!STEP1_FACT_SUMMARY_PROMPT.is_empty());
        assert!(!STEP2_USER_PROFILE_PROMPT.is_empty());
        assert!(!STEP3_PITFALL_PROMPT.is_empty());
        assert!(!STEP4_EVOLUTION_PROMPT.is_empty());
    }

    #[test]
    fn build_l1_to_l2_prompt_substitutes() {
        let prompt = build_l1_to_l2_prompt("[{\"id\":\"r1\"}]");
        assert!(prompt.contains("[{\"id\":\"r1\"}]"));
        assert!(!prompt.contains("{raw_entries_json}"));
    }

    #[test]
    fn build_l2_to_l3_prompt_substitutes() {
        let prompt = build_l2_to_l3_prompt("debugging", "[{\"id\":\"idx-1\"}]");
        assert!(prompt.contains("debugging"));
        assert!(prompt.contains("[{\"id\":\"idx-1\"}]"));
        assert!(!prompt.contains("{category}"));
        assert!(!prompt.contains("{index_entries_json}"));
    }

    #[test]
    fn step1_prompt_substitutes() {
        let prompt = build_step1_prompt("[{\"role\":\"user\"}]", None);
        assert!(prompt.contains("[{\"role\":\"user\"}]"));
        assert!(prompt.contains("首次总结"));
        assert!(!prompt.contains("{conversation_json}"));
    }

    #[test]
    fn step1_prompt_with_previous_summary() {
        let prompt = build_step1_prompt("[{\"role\":\"user\"}]", Some("之前讨论了 Rust 架构"));
        assert!(prompt.contains("[{\"role\":\"user\"}]"));
        assert!(prompt.contains("之前讨论了 Rust 架构"));
        assert!(prompt.contains("已有的事实摘要"));
        assert!(!prompt.contains("首次总结"));
    }

    #[test]
    fn step2_prompt_substitutes() {
        let prompt = build_step2_prompt(
            "用户在开发",
            "[{\"msg\":\"hello\"}]",
            "Rust",
            "简洁代码",
            "不要GC",
            "TDD",
        );
        assert!(prompt.contains("用户在开发"));
        assert!(prompt.contains("Rust"));
        assert!(prompt.contains("简洁代码"));
        assert!(!prompt.contains("{fact_summary}"));
        assert!(!prompt.contains("{existing_explicit}"));
    }

    #[test]
    fn step3_prompt_substitutes() {
        let prompt = build_step3_prompt("做了重构", "[{\"msg\":\"err\"}]", "已有坑");
        assert!(prompt.contains("做了重构"));
        assert!(prompt.contains("已有坑"));
        assert!(!prompt.contains("{fact_summary}"));
        assert!(!prompt.contains("{existing_pitfalls}"));
    }

    #[test]
    fn step4_prompt_substitutes() {
        let prompt = build_step4_prompt("[{\"id\":\"p1\"}]", "已有规则");
        assert!(prompt.contains("[{\"id\":\"p1\"}]"));
        assert!(prompt.contains("已有规则"));
        assert!(!prompt.contains("{pitfalls_json}"));
        assert!(!prompt.contains("{existing_rules}"));
    }

    /// ── 场景模拟：DeepSeek 配置踩坑 → Step5 叙事编辑 ──
    #[test]
    fn step5_scenario_deepseek_config() {
        let fact_summary = "\
用户想将DeepSeek最新模型配置到Claude Code中使用。
助手查询了DeepSeek官网和Claude Code配置文档，找到了配置方法。
助手在本机找到了多个配置文件：settings.json、settings.local.json等。
助手未向用户确认，自行判断修改了settings.local.json。
用户反馈配置未生效。用户指出是修改了错误的文件。
助手最终修改了settings.json，配置生效。";

        let pitfalls_text = "\
- [ToolFailure] 修改配置文件时未确认正确的文件路径
- [LazyBehavior] 未主动向用户确认要修改哪个配置文件";

        let evolution_text = "\
- 修改配置文件前必须先向用户确认正确的文件路径";

        let existing_narrative = "无，这是首次生成";

        let prompt = build_step5_prompt(
            fact_summary,
            pitfalls_text,
            evolution_text,
            existing_narrative,
        );

        // 验证新 prompt 包含叙事编辑器身份
        assert!(
            prompt.contains("记忆叙事编辑器"),
            "新 prompt 应包含叙事编辑器身份"
        );
        assert!(prompt.contains("准入门槛"));
        assert!(prompt.contains("narrative"));
        assert!(prompt.contains("new_keywords"));
        // 不应包含旧的 entries 数组格式
        assert!(!prompt.contains("\"entries\""));

        // 模拟 LLM 返回叙事更新
        let llm_response = serde_json::json!({
            "narrative": "配过Claude Code的DeepSeek模型，踩过改错配置文件的坑",
            "new_keywords": ["DeepSeek", "配置文件", "settings.json", "Claude Code"]
        });
        let response_str = serde_json::to_string(&llm_response).unwrap();

        // 解析
        let parsed: serde_json::Value = serde_json::from_str(&response_str).unwrap();
        let narrative = parsed
            .get("narrative")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let new_keywords: Vec<String> = parsed
            .get("new_keywords")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();

        assert!(!narrative.is_empty(), "叙事不应为空");
        assert!(narrative.contains("配置文件"), "叙事应包含关键信息");
        assert!(new_keywords.len() >= 3, "应提取足够关键词");
    }

    /// ── 场景模拟：已有叙事，网文提示词+记忆脑补充 ──
    #[test]
    fn step5_scenario_novel_prompt_merge() {
        let fact_summary = "用户开发了 ai brain 记忆脑的四步分析和三层存储架构";
        let pitfalls_text = "（无新增）";
        let evolution_text = "";
        let existing_narrative = "优化过网文写作提示词（6部分结构），了解AI味的典型特征";

        let prompt = build_step5_prompt(
            fact_summary,
            pitfalls_text,
            evolution_text,
            existing_narrative,
        );

        // 验证已有叙事被传入
        assert!(prompt.contains("优化过网文写作提示词"));
        assert!(prompt.contains("6部分结构"));

        // 模拟 LLM 合并输出
        let llm_response = serde_json::json!({
            "narrative": "优化过网文写作提示词（6部分结构），了解AI味的典型特征，开发过AI Brain记忆脑（四步分析+三层存储）",
            "new_keywords": ["记忆脑", "四步分析", "三层存储"]
        });

        let parsed: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&llm_response).unwrap()).unwrap();
        let narrative = parsed
            .get("narrative")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // 验证两个领域都在叙事中
        assert!(narrative.contains("提示词"), "应保留旧领域");
        assert!(narrative.contains("记忆脑"), "应包含新领域");
        // 不是简单末尾追加，而是相关内容自然融合
        assert!(narrative.contains("四步分析"));
    }

    #[test]
    fn step5_prompt_empty_narrative() {
        let prompt = build_step5_prompt("事实", "踩坑", "规则", "无，这是首次生成");
        assert!(prompt.contains("无，这是首次生成"));
        assert!(!prompt.contains("{existing_narrative}"));
    }

    #[test]
    fn step5_prompt_existing_narrative() {
        let prompt = build_step5_prompt("新事实", "新踩坑", "新规则", "做过X，做过Y");
        assert!(prompt.contains("做过X，做过Y"));
        assert!(!prompt.contains("{existing_narrative}"));
    }

    #[test]
    fn step5_prompt_all_placeholders_replaced() {
        let prompt = build_step5_prompt("fact", "pitfall", "evolution", "existing");
        assert!(!prompt.contains("{fact_summary}"));
        assert!(!prompt.contains("{pitfalls_text}"));
        assert!(!prompt.contains("{evolution_text}"));
        assert!(!prompt.contains("{existing_narrative}"));
    }

    // ── 金字塔浓缩 prompt 测试 ──

    #[test]
    fn concentration_step1_prompt_substitutes() {
        let prompt = build_concentration_step1_prompt(
            "[{\"role\":\"user\",\"content\":\"修复TUI鼠标\"}]",
            "[{\"task_id\":\"t-old\",\"summary\":\"旧任务\"}]",
        );
        assert!(prompt.contains("TUI鼠标"));
        assert!(prompt.contains("旧任务"));
        assert!(!prompt.contains("{conversation_json}"));
        assert!(!prompt.contains("{existing_l2_index}"));
    }

    #[test]
    fn concentration_step2_prompt_substitutes() {
        let prompt = build_concentration_step2_prompt(
            "[{\"task_id\":\"t-1\",\"summary\":\"编码\"}]",
            "[{\"task_type\":\"Coding\",\"experiences\":[]}]",
        );
        assert!(prompt.contains("编码"));
        assert!(prompt.contains("Coding"));
        assert!(!prompt.contains("{l2_data}"));
        assert!(!prompt.contains("{existing_l3}"));
    }

    #[test]
    fn concentration_step3_prompt_substitutes() {
        let prompt = build_concentration_step3_prompt(
            "[{\"task_type\":\"Coding\",\"experiences\":[]}]",
            "{\"triggers\":[],\"narrative\":\"旧叙事\"}",
        );
        assert!(prompt.contains("旧叙事"));
        assert!(!prompt.contains("{l3_data}"));
        assert!(!prompt.contains("{existing_l4}"));
    }

    #[test]
    fn concentration_step4_prompt_substitutes() {
        let prompt = build_concentration_step4_prompt(
            "[{\"role\":\"user\"}]",
            "Rust开发者",
            "要测试\n别忘判空",
        );
        assert!(prompt.contains("Rust开发者"));
        assert!(prompt.contains("要测试"));
        assert!(!prompt.contains("{conversation_json}"));
        assert!(!prompt.contains("{existing_profile}"));
        assert!(!prompt.contains("{existing_eval_info}"));
    }
}

// ===========================================================================
// 金字塔四步浓缩 Prompt（新增，替代旧的 8 步分析）
// ===========================================================================

/// 第一步：L1→L2 任务拆分（全量重生成）
///
/// 将原始对话拆分为独立任务摘要，跨会话合并同类任务。
pub const CONCENTRATION_STEP1_PROMPT: &str = "\
# 身份
你是一个对话分析引擎。你将原始对话内容拆分为独立的任务，跨会话合并同类任务。

# 输入
新会话原始对话（JSON 数组）：
{conversation_json}

现有 L2 任务索引：
{existing_l2_index}

# 规则
1. 将对话拆分为独立任务（一个会话可拆出多个任务，多个会话的任务可合并）
2. 每个任务需分类到以下类型之一：Coding, Writing, Troubleshooting, Research, Multimedia, Configuration, Other
3. 合并：新对话中的内容如果与已有任务属于同类工作，合并到同一任务中（更新 summary）
4. 每个任务提供：
   - task_id: 唯一标识（已有任务保留原 ID，新任务用 new-1, new-2...）
   - task_type: 任务类型
   - task_name: 简短名称（10字以内）
   - summary: 2-3 句话概括（200字以内）
   - l1_refs: 关联的会话段落索引
   - tags: 3-5 个关键词
   - importance: 0.0-1.0
5. 最多保留 50 个任务，超过请合并相关任务
6. 用中文输出

# 输出格式（严格 JSON 数组）
[
  {
    \"task_id\": \"task-001\",
    \"task_type\": \"Coding\",
    \"task_name\": \"TUI鼠标修复\",
    \"summary\": \"修复EnableMouseCapture拦截导致终端原生选择失效...\",
    \"l1_refs\": [{\"session\": \"sess-xxx\", \"paragraphs\": [3, 4]}],
    \"tags\": [\"TUI\", \"鼠标\", \"crossterm\"],
    \"importance\": 0.85
  }
]\
";

/// 第二步：L2→L3 经验抽象（全量重生成）
///
/// 从任务摘要中提炼按类型汇总的经验。
pub const CONCENTRATION_STEP2_PROMPT: &str = "\
# 身份
你是一个经验提炼引擎。你从任务摘要中提炼出按类型汇总的可复用经验。

# 输入
L2 任务摘要数据：
{l2_data}

现有 L3 经验数据：
{existing_l3}

# 规则
1. 按任务类型（Coding/Writing/Troubleshooting/Research/等）分组提炼经验
2. 每条经验包括：
   - pattern: 经验模式名称（如「工具调用失败时的替代方案」）
   - description: 具体描述（100字以内）
   - source_tasks: 来源任务 ID 列表
   - frequency: 出现频率
   - injectable: 是否在启动时注入上下文（只有高频且通用的经验设为 true）
3. 每个类型最多保留 10 条经验，超过请合并浓缩
4. 为每个类型生成关键词索引（keyword → 关联的 L2 任务 ID）
5. 合并：新经验与已有经验重合时，融合为更精炼的一条
6. 用中文输出

# 输出格式（严格 JSON 数组，每个类型一个对象）
[
  {
    \"task_type\": \"Coding\",
    \"experiences\": [
      {
        \"pattern\": \"工具调用失败时的替代方案\",
        \"description\": \"WebSearch工具调用失败时，可用bash+curl替代获取网页内容\",
        \"source_tasks\": [\"task-001\", \"task-007\"],
        \"frequency\": 3,
        \"injectable\": true
      }
    ],
    \"l2_refs\": [\"task-001\", \"task-007\"],
    \"index\": [{\"keyword\": \"工具替代\", \"l2_task_ids\": [\"task-001\"]}]
  }
]\
";

/// 第三步：L3→L4 触发词提取（全量重生成）
///
/// 从 L3 经验中提取触发词和叙事文本。
pub const CONCENTRATION_STEP3_PROMPT: &str = "\
# 身份
你是一个触发词提取引擎。你从 L3 经验中提取关键触发词和一段关于用户的流动叙事。

# 输入
L3 经验数据：
{l3_data}

现有 L4 潜意识数据：
{existing_l4}

# 规则
1. 从 L3 经验中提取触发词（关键词短语，能触发相关记忆的召回）
2. 每个触发词指向一个 L3 类型和 L2 任务
3. 叙事文本：用最少的文字描述用户做过什么、擅长什么、踩过什么坑
4. 容量限制：
   - 触发词最多 50 个（超过请合并或删除低价值项）
   - 叙事文本最多 500 字
5. 叙事编辑原则：
   - 合并：相关领域经验融合为一句
   - 覆盖：新信息推翻旧结论时自然替换
   - 精炼：用逗号/顿号连接短语，追求最少文字×最大覆盖
6. 只有真正有价值的经验才值得设为触发词
7. 用中文输出

# 输出格式（严格 JSON）
{
  \"triggers\": [
    {\"keyword\": \"红冲逻辑变更\", \"l3_type\": \"Coding\", \"l2_task\": \"task-033\"},
    {\"keyword\": \"TUI鼠标选择\", \"l3_type\": \"Coding\", \"l2_task\": \"task-001\"}
  ],
  \"narrative\": \"用户是Rust全栈开发者，偏好极简指令。完成过Nexus红冲逻辑、消消乐游戏。\"
}\
";

/// 第四步：Profile + EvalInfo（全量重生成）
///
/// 生成 100 字画像和评估信息。
pub const CONCENTRATION_STEP4_PROMPT: &str = "\
# 身份
你是一个用户画像和评估信息生成引擎。你从对话中提炼精炼的用户画像和评估信息。

# 输入
对话记录：
{conversation_json}

现有用户画像：
{existing_profile}

现有评估信息：
{existing_eval_info}

# 规则

## 用户画像
1. 用 100 字以内的自然语言描述用户：身份、擅长、偏好、工作模式
2. 不是罗列特征，而是一段连贯的描述文本
3. 融合已有画像和新对话中的信息

## 评估信息
为评估脑提供三个列表：
1. requirements（评估要求）：用户对输出质量的要求（最多 5 条）
   - 合并语义重复的要求
   - 只保留最重要、最高频的要求
2. pitfalls（已知踩坑）：系统犯过的典型错误（最多 5 条）
   - 每条简洁描述错误和正确做法
3. rules（进化规则）：可执行的改进规则（最多 3 条）
   - 格式：\"当 [条件] 时，[行为]\"
   - 来自具体踩坑记录

4. 用中文输出

# 输出格式（严格 JSON）
{
  \"profile\": \"用户是Rust全栈开发者，偏好简洁指令式交互，零容忍偏离指令的行为\",
  \"requirements\": [\"回答必须包含具体代码示例\"],
  \"pitfalls\": [\"修改配置文件前未确认正确路径\"],
  \"rules\": [\"当修改配置文件时，先向用户确认正确的文件路径\"]
}\
";

// ---------------------------------------------------------------------------
// 金字塔浓缩 Prompt Builder
// ---------------------------------------------------------------------------

/// 构建第一步：L1→L2 任务拆分 prompt
pub fn build_concentration_step1_prompt(
    conversation_json: &str,
    existing_l2_index: &str,
) -> String {
    CONCENTRATION_STEP1_PROMPT
        .replace("{conversation_json}", conversation_json)
        .replace("{existing_l2_index}", existing_l2_index)
}

/// 构建第二步：L2→L3 经验抽象 prompt
pub fn build_concentration_step2_prompt(l2_data: &str, existing_l3: &str) -> String {
    CONCENTRATION_STEP2_PROMPT
        .replace("{l2_data}", l2_data)
        .replace("{existing_l3}", existing_l3)
}

/// 构建第三步：L3→L4 触发词提取 prompt
pub fn build_concentration_step3_prompt(l3_data: &str, existing_l4: &str) -> String {
    CONCENTRATION_STEP3_PROMPT
        .replace("{l3_data}", l3_data)
        .replace("{existing_l4}", existing_l4)
}

/// 构建第四步：Profile + EvalInfo prompt
pub fn build_concentration_step4_prompt(
    conversation_json: &str,
    existing_profile: &str,
    existing_eval_info: &str,
) -> String {
    CONCENTRATION_STEP4_PROMPT
        .replace("{conversation_json}", conversation_json)
        .replace("{existing_profile}", existing_profile)
        .replace("{existing_eval_info}", existing_eval_info)
}

// ===========================================================================
// 金字塔浓缩 Prompt — 拆分版（system 稳定 + user 变化，优化 KV Cache）
// ===========================================================================

/// Step1 系统指令（稳定，KV Cache 可命中）
pub const CONCENTRATION_STEP1_SYSTEM: &str = "\
# 身份
你是一个对话分析引擎。你将原始对话内容拆分为独立的任务，跨会话合并同类任务。

# 规则
1. 将对话拆分为独立任务（一个会话可拆出多个任务，多个会话的任务可合并）
2. 每个任务需分类到以下类型之一：Coding, Writing, Troubleshooting, Research, Multimedia, Configuration, Other
3. 合并：新对话中的内容如果与已有任务属于同类工作，合并到同一任务中（更新 summary）
4. 每个任务提供：task_id, task_type, task_name, summary, l1_refs, tags, importance
5. 最多保留 50 个任务，超过请合并相关任务
6. 用中文输出

# 输出格式（严格 JSON 数组）
[
  {
    \"task_id\": \"task-001\",
    \"task_type\": \"Coding\",
    \"task_name\": \"TUI鼠标修复\",
    \"summary\": \"修复EnableMouseCapture拦截导致终端原生选择失效...\",
    \"l1_refs\": [{\"session\": \"sess-xxx\", \"paragraphs\": [3, 4]}],
    \"tags\": [\"TUI\", \"鼠标\", \"crossterm\"],
    \"importance\": 0.85
  }
]\
";

/// Step2 系统指令（稳定）
pub const CONCENTRATION_STEP2_SYSTEM: &str = "\
# 身份
你是一个经验提炼引擎。你从任务摘要中提炼出按类型汇总的可复用经验。

# 规则
1. 按任务类型分组提炼经验
2. 每条经验包括：pattern, description, source_tasks, frequency, injectable
3. 每个类型最多保留 10 条经验
4. 为每个类型生成关键词索引
5. 合并：新经验与已有经验重合时融合为更精炼的一条
6. 用中文输出

# 输出格式（严格 JSON 数组，每个类型一个对象）
[
  {
    \"task_type\": \"Coding\",
    \"experiences\": [{\"pattern\": \"...\",\"description\": \"...\",\"source_tasks\": [],\"frequency\": 1,\"injectable\": false}],
    \"l2_refs\": [],
    \"index\": [{\"keyword\": \"...\",\"l2_task_ids\": []}]
  }
]\
";

/// Step3 系统指令（稳定）
pub const CONCENTRATION_STEP3_SYSTEM: &str = "\
# 身份
你是一个触发词提取引擎。你从 L3 经验中提取关键触发词和一段关于用户的流动叙事。

# 规则
1. 从 L3 经验中提取触发词（关键词短语，能触发相关记忆的召回）
2. 每个触发词指向一个 L3 类型和 L2 任务
3. 叙事文本：用最少的文字描述用户做过什么、擅长什么、踩过什么坑
4. 容量限制：触发词最多 50 个，叙事文本最多 500 字
5. 只有真正有价值的经验才值得设为触发词
6. 用中文输出

# 输出格式（严格 JSON）
{
  \"triggers\": [{\"keyword\": \"...\",\"l3_type\": \"Coding\",\"l2_task\": \"task-xxx\"}],
  \"narrative\": \"用户是...的开发者\"
}\
";

/// Step4 系统指令（稳定）
pub const CONCENTRATION_STEP4_SYSTEM: &str = "\
# 身份
你是一个用户画像和评估信息生成引擎。你从对话中提炼精炼的用户画像和评估信息。

# 规则
## 用户画像
1. 用 100 字以内自然语言描述用户：身份、擅长、偏好、工作模式
2. 融合已有画像和新对话中的信息

## 评估信息
1. requirements（评估要求）：最多 5 条
2. pitfalls（已知踩坑）：最多 5 条
3. rules（进化规则）：最多 3 条，格式：\"当 [条件] 时，[行为]\"
4. 用中文输出

# 输出格式（严格 JSON）
{
  \"profile\": \"用户是...\",
  \"requirements\": [\"...\"],
  \"pitfalls\": [\"...\"],
  \"rules\": [\"...\"]
}\
";

/// 构建第一步拆分 prompt（返回 system, user）
pub fn build_concentration_step1_split(
    conversation_json: &str,
    existing_l2_index: &str,
) -> (&'static str, String) {
    let user = format!(
        "# 输入数据\n\n新会话原始对话（JSON 数组）：\n{conversation_json}\n\n现有 L2 任务索引：\n{existing_l2_index}"
    );
    (CONCENTRATION_STEP1_SYSTEM, user)
}

/// 构建第二步拆分 prompt（返回 system, user）
pub fn build_concentration_step2_split(l2_data: &str, existing_l3: &str) -> (&'static str, String) {
    let user = format!(
        "# 输入数据\n\nL2 任务摘要数据：\n{l2_data}\n\n现有 L3 经验数据：\n{existing_l3}"
    );
    (CONCENTRATION_STEP2_SYSTEM, user)
}

/// 构建第三步拆分 prompt（返回 system, user）
pub fn build_concentration_step3_split(l3_data: &str, existing_l4: &str) -> (&'static str, String) {
    let user = format!(
        "# 输入数据\n\nL3 经验数据：\n{l3_data}\n\n现有 L4 潜意识数据：\n{existing_l4}"
    );
    (CONCENTRATION_STEP3_SYSTEM, user)
}

/// 构建第四步拆分 prompt（返回 system, user）
pub fn build_concentration_step4_split(
    conversation_json: &str,
    existing_profile: &str,
    existing_eval_info: &str,
) -> (&'static str, String) {
    let user = format!(
        "# 输入数据\n\n对话记录：\n{conversation_json}\n\n现有用户画像：\n{existing_profile}\n\n现有评估信息：\n{existing_eval_info}"
    );
    (CONCENTRATION_STEP4_SYSTEM, user)
}
