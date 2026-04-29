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

/// 第一步：事实总结（做了什么）
pub const STEP1_FACT_SUMMARY_PROMPT: &str = "\
# 身份
你是一个对话分析引擎。你分析最近几轮的对话记录，总结出事实性摘要。

# 输入
以下是对话记录（JSON 数组）：
{conversation_json}

# 规则
1. 只总结客观事实：用户做了什么、讨论了什么、决定了什么
2. 保留所有技术细节：文件路径、命令、配置值、错误信息
3. 按时间顺序组织，形成连贯的叙事
4. 不要遗漏关键步骤
5. 用中文输出
6. 摘要长度控制在 300 字以内

# 输出格式
直接输出事实总结文本（纯文本，不要 JSON 包裹）。\
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

/// 构建第一步：事实总结 prompt
pub fn build_step1_prompt(conversation_json: &str) -> String {
    STEP1_FACT_SUMMARY_PROMPT.replace("{conversation_json}", conversation_json)
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

/// 构建第五步：潜意识抽象 prompt
pub fn build_step5_prompt(
    fact_summary: &str,
    pitfalls_text: &str,
    evolution_text: &str,
    existing_subconscious: &str,
) -> String {
    STEP5_SUBCONSCIOUS_PROMPT
        .replace("{fact_summary}", fact_summary)
        .replace("{pitfalls_text}", pitfalls_text)
        .replace("{evolution_text}", evolution_text)
        .replace("{existing_subconscious}", existing_subconscious)
}

/// 第五步：潜意识抽象（印象索引生成）
///
/// 三层渐进披露设计：
/// - impression: 极简"我做过这件事"（触发用）
/// - pitfall_hint: 直觉级"有个坑大概是这样"（唤醒用）
/// - reference_hint: L2/L3 具体文件引用（深入回忆用）
pub const STEP5_SUBCONSCIOUS_PROMPT: &str = "\
# 身份
你是一个记忆抽象引擎。你将对话事实、踩坑记录、进化规则抽象为「潜意识印象」——三层渐进披露。

# 输入
事实总结：
{fact_summary}

本轮新增踩坑：
{pitfalls_text}

本轮新增进化规则：
{evolution_text}

已有潜意识印象（去重用）：
{existing_subconscious}

# 准入门槛（关键！不是所有对话都值得存潜意识）
只有满足以下至少一项的内容才能生成潜意识条目：
✅ 踩坑/故障排查经验（遇到什么问题、怎么解决的）
✅ 架构决策或技术选型（为什么这么设计）
✅ 用户明确的偏好/禁忌/工作习惯
✅ 跨项目可复用的经验教训
✅ 复杂业务逻辑的关键理解

以下内容【禁止】存入潜意识：
❌ 一次性闲聊（称呼、玩笑、寒暄）
❌ 通用编程知识（语法、标准库用法）
❌ 当前对话的上下文信息（这些属于会话记忆，不是潜意识）
❌ 自我介绍/功能说明
❌ 浅层问答（查个信息、问个概念）
❌ 对自身能力的描述（那不是经验，是自我认知）

# 三层渐进披露规则（关键！）
1. topic：主题领域，简短（如\"Claude Code配置\"，不是\"Claude Code配置文件修改踩坑\"）
2. trigger_keywords：触发词，用户提到这些词时说明可能做过相关的事，最多 8 个
3. impression：极简印象，只说\"了解过/做过/配置过/开发过X\"，不超过 15 字，不写结论和教训
4. pitfall_hint：踩坑摘要，一句话直觉级描述坑在哪（如\"配置文件有多个，改错了\"），没有踩坑则为空字符串
5. reference_hint：精确引用，指向有详情的文件（如\"pitfall/pt-xxx.json\"、\"sessions/sess_xxx\"），只填目录级即可
6. 每个 topic 独立一条，不要把不相关的事情合并
7. importance 评估：有踩坑记录且有进化规则 → 0.9+，仅有踩坑 → 0.7-0.8，仅有事实 → 0.5 以下或不生成
8. 如果本轮对话没有产生有价值的新经验，返回空 entries 数组
9. 用中文输出

# 输出格式（严格 JSON）
{
  \"entries\": [
    {
      \"topic\": \"Claude Code配置\",
      \"trigger_keywords\": [\"配置文件\", \"settings\", \"Claude Code\", \"修改配置\"],
      \"impression\": \"了解过Claude Code配置\",
      \"pitfall_hint\": \"配置文件有多个，改错了文件\",
      \"reference_hint\": \"pitfall/\",
      \"importance\": 0.85
    }
  ]
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

已有潜意识印象（参考用）：
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
        let prompt = build_step1_prompt("[{\"role\":\"user\"}]");
        assert!(prompt.contains("[{\"role\":\"user\"}]"));
        assert!(!prompt.contains("{conversation_json}"));
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

    /// ── 场景模拟：DeepSeek 配置 4 轮对话 → Step5 潜意识抽取 ──
    ///
    /// 对话内容：
    ///   R1: 用户想配 DeepSeek 到 Claude Code → 助手查了官网和配置文档
    ///   R2: 用户不想自己配 → 助手直接改了 settings.local.json（未确认）
    ///   R3: 配置没生效 → 助手排查内容正确但方向错了
    ///   R4: 用户指出改错文件了 → 助手改到 settings.json 修复
    #[test]
    fn step5_scenario_deepseek_config() {
        // ── Step1 产出：事实总结 ──
        let fact_summary = "\
用户想将DeepSeek最新模型配置到Claude Code中使用。
助手查询了DeepSeek官网和Claude Code配置文档，找到了配置方法。
助手在本机找到了多个配置文件：settings.json、settings.local.json等。
助手未向用户确认，自行判断修改了settings.local.json。
用户反馈配置未生效。助手排查了配置内容本身，确认格式和参数都正确。
用户指出是修改了错误的文件，应该是settings.json而非settings.local.json。
助手最终修改了settings.json，配置生效。";

        // ── Step3 产出：踩坑记录 ──
        let pitfalls_text = "\
- [ToolFailure] 修改配置文件时未确认正确的文件路径，自行假设settings.local.json是正确的文件
- [WrongAnswer] 排查配置不生效问题时，只检查了配置内容是否正确，未优先检查是否修改了正确的文件路径
- [LazyBehavior] 未主动向用户确认要修改哪个配置文件，擅自做了判断";

        // ── Step4 产出：进化规则 ──
        let evolution_text = "\
- 修改配置文件前必须先向用户确认正确的文件路径，不能自行假设
- 排查配置不生效问题时，应优先检查是否修改了正确的文件，而非只检查内容
- 当存在多个同名/相似配置文件时，必须逐一确认用途后再操作";

        let existing_subconscious = "（无）";

        // ── 构建 Step5 prompt ──
        let prompt = build_step5_prompt(
            fact_summary,
            pitfalls_text,
            evolution_text,
            existing_subconscious,
        );

        // 打印完整 prompt 供人工审查
        eprintln!("\n========== Step5 Prompt (新版本) ==========\n{prompt}\n========== End ==========\n");

        // 验证准入门槛存在于 prompt 中
        assert!(prompt.contains("准入门槛"), "新 prompt 应包含准入门槛");
        assert!(prompt.contains("踩坑"), "准入标准应包含踩坑经验");
        assert!(prompt.contains("禁止"), "应包含禁止标准");
        assert!(prompt.contains("一次性闲聊"), "禁止标准应包含一次性闲聊");
        assert!(prompt.contains("通用编程知识"), "禁止标准应包含通用编程知识");

        // ── 模拟 LLM 返回（按新 prompt 三层渐进披露） ──
        // 新 prompt 要求：impression 极简 + pitfall_hint 直觉级 + reference_hint 精确引用
        let llm_response = serde_json::json!({
            "entries": [{
                "topic": "Claude Code配置",
                "trigger_keywords": ["配置文件", "settings.json", "Claude Code", "修改配置", "配置不生效"],
                "impression": "了解过Claude Code配置",
                "pitfall_hint": "配置文件有多个，改错了文件",
                "reference_hint": "pitfall/",
                "importance": 0.85
            }]
        });
        let response_str = serde_json::to_string(&llm_response).unwrap();

        // ── 解析 LLM 返回（复用 step5 的解析逻辑） ──
        let parsed: serde_json::Value = serde_json::from_str(&response_str).unwrap();
        let entries_arr = parsed
            .get("entries")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut new_entries: Vec<crate::subconscious::NewSubconsciousEntry> = Vec::new();
        for e in &entries_arr {
            let topic = e.get("topic").and_then(|v: &serde_json::Value| v.as_str()).unwrap_or("").to_string();
            if topic.is_empty() {
                continue;
            }
            let trigger_keywords: Vec<String> = e
                .get("trigger_keywords")
                .and_then(|v: &serde_json::Value| v.as_array())
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|v: serde_json::Value| v.as_str().map(String::from))
                .filter(|s: &String| !s.trim().is_empty())
                .take(8)
                .collect();
            let impression = e.get("impression").and_then(|v: &serde_json::Value| v.as_str()).unwrap_or("").to_string();
            let pitfall_hint = e.get("pitfall_hint").and_then(|v: &serde_json::Value| v.as_str()).unwrap_or("").to_string();
            let reference_hint = e.get("reference_hint").and_then(|v: &serde_json::Value| v.as_str()).unwrap_or("").to_string();
            let importance = e.get("importance").and_then(|v: &serde_json::Value| v.as_f64()).unwrap_or(0.7);

            new_entries.push(crate::subconscious::NewSubconsciousEntry {
                topic,
                trigger_keywords,
                impression,
                pitfall_hint,
                reference_hint,
                importance,
            });
        }

        // ── 验证结果 ──
        eprintln!("\n========== 潜意识抽取结果（三层渐进披露） ==========");
        for entry in &new_entries {
            eprintln!(
                "  topic: {}\n  impression: {}  ← 极简触发\n  pitfall_hint: {}  ← 直觉级踩坑\n  reference: {}  ← 精确引用\n  importance: {}\n",
                entry.topic, entry.impression, entry.pitfall_hint, entry.reference_hint, entry.importance
            );
        }

        // 应该只有 1 条（不是 4 条每轮一条）
        assert_eq!(new_entries.len(), 1, "应该只生成 1 条高质量潜意识");
        assert_eq!(new_entries[0].topic, "Claude Code配置");
        assert!(new_entries[0].importance >= 0.7, "有踩坑+进化规则 importance 应 >= 0.7");
        assert!(new_entries[0].trigger_keywords.len() >= 3);

        // impression 极简（<= 15 字）
        assert!(
            new_entries[0].impression.chars().count() <= 20,
            "impression 应极简，实际: {} ({}字)",
            new_entries[0].impression,
            new_entries[0].impression.chars().count()
        );
        assert!(
            new_entries[0].impression.contains("了解") || new_entries[0].impression.contains("做过"),
            "impression 应只表达'我做过这事'"
        );

        // pitfall_hint 非空且简短直觉
        assert!(
            !new_entries[0].pitfall_hint.is_empty(),
            "有踩坑时 pitfall_hint 不应为空"
        );
        assert!(
            new_entries[0].pitfall_hint.contains("改错"),
            "pitfall_hint 应直觉级描述坑"
        );

        // 不应包含结论性内容（那是 L2/L3 的活）
        assert!(
            !new_entries[0].impression.contains("必须"),
            "impression 不应包含结论性指令"
        );
        assert!(
            !new_entries[0].impression.contains("教训"),
            "impression 不应包含教训总结"
        );

        // ── 模拟 load_subconscious_summary 过滤 ──
        let all_passed_filter = new_entries.iter().all(|e| e.importance >= 0.5);
        assert!(all_passed_filter, "所有条目应通过 importance >= 0.5 过滤");
        assert!(new_entries.len() <= 6, "条目数应 <= 6");

        eprintln!("========== 过滤验证通过 ==========\n");
    }

    /// ── 对照组：旧 prompt 会生成什么垃圾 ──
    ///
    /// 旧 prompt 只说"从中提取做过的事情"，LLM 大概率会生成 3-4 条：
    /// 1. DeepSeek模型配置查询（浅层信息查询，无价值）
    /// 2. Claude Code配置文件查找（通用操作，无价值）
    /// 3. 配置文件修改操作（重复了，无价值）
    /// 4. 配置不生效排查（和#3重复）
    /// 而"角色称呼"这类直接被忽略（本轮没有）
    #[test]
    fn step5_old_prompt_would_produce_garbage() {
        let _fact_summary = "用户想配置DeepSeek最新模型到Claude Code。\
助手查询了DeepSeek官网。助手修改了settings.local.json。\
用户说配置没生效。最终改了settings.json。";

        // 旧 prompt 没有准入门槛，LLM 会把一切"做过的事"都提取
        // 模拟旧 prompt 下 LLM 的典型输出：
        let old_llm_response = serde_json::json!({
            "entries": [
                {
                    "topic": "DeepSeek模型配置",
                    "trigger_keywords": ["DeepSeek", "模型配置", "最新模型", "Claude Code"],
                    "impression": "帮用户查询过DeepSeek最新模型的配置方法",
                    "reference_hint": "sessions/",
                    "importance": 0.6
                },
                {
                    "topic": "配置文件查找",
                    "trigger_keywords": ["配置文件", "settings", "查找文件"],
                    "impression": "查找过Claude Code的配置文件，找到settings.json和settings.local.json",
                    "reference_hint": "sessions/",
                    "importance": 0.5
                },
                {
                    "topic": "配置文件修改",
                    "trigger_keywords": ["修改配置", "settings.local", "settings.json"],
                    "impression": "修改过Claude Code配置文件",
                    "reference_hint": "sessions/",
                    "importance": 0.5
                },
                {
                    "topic": "配置不生效排查",
                    "trigger_keywords": ["配置不生效", "排查", "不生效"],
                    "impression": "排查过配置不生效的问题",
                    "reference_hint": "sessions/",
                    "importance": 0.5
                }
            ]
        });

        let entries = old_llm_response.get("entries").unwrap().as_array().unwrap();

        eprintln!("\n========== 旧 Prompt 典型输出 (4条垃圾) ==========");
        for e in entries {
            eprintln!(
                "  topic: {} | importance: {} | impression: {}",
                e["topic"].as_str().unwrap(),
                e["importance"].as_f64().unwrap(),
                e["impression"].as_str().unwrap()
            );
        }

        // 旧 prompt 生成 4 条，新 prompt 应只生成 1 条
        assert_eq!(entries.len(), 4, "旧 prompt 会生成 4 条");
        eprintln!("========== 对比：新 prompt 只生成 1 条高质量条目 ==========\n");
    }
}
