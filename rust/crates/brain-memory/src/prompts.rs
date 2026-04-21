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
}
