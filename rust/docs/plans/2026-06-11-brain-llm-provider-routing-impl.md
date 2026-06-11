# 每脑独立 LLM Provider 路由 — 实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 让每个副脑可以使用不同厂商的 LLM，统一上下文构造方式以最大化 KV Cache 前缀命中率。

**Architecture:** 在 `LlmSection` 新增 `brain_providers` HashMap 实现脑→厂商映射。`AnalysisLlm` trait 升级为 `analyze_structured(system, user)` 双参数接口，将稳定指令模板与变化数据分离。`ThresholdCompressor` 同步拆分 prompt。默认厂商改为小米 MiMo。

**Tech Stack:** Rust, serde (TOML), tokio (async trait), brain-llm crate

**设计文档:** `docs/plans/2026-06-11-brain-llm-provider-routing-design.md`

---

### Task 1: 配置层 — `brain_providers` 字段 + `provider_for_brain()` + `create_brain_client()` 改路由

**Files:**
- Modify: `crates/brain-llm/src/config.rs:21-34` (LlmSection)
- Modify: `crates/brain-llm/src/config.rs:110-182` (impl LlmConfig)
- Test: `crates/brain-llm/src/config.rs:275-369` (mod tests)

**Step 1: 在 `LlmSection` 新增 `brain_providers` 字段**

在 `crates/brain-llm/src/config.rs` 的 `LlmSection` 结构体中，在 `brain_params` 和 `defaults` 之间新增：

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmSection {
    pub default_provider: String,
    pub default_model: String,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub brain_models: HashMap<String, String>,
    #[serde(default)]
    pub brain_params: HashMap<String, BrainParams>,
    /// 每脑独立厂商映射（未配置的脑走 default_provider）
    #[serde(default)]
    pub brain_providers: HashMap<String, String>,
    #[serde(default)]
    pub defaults: LlmDefaults,
}
```

**Step 2: 新增 `provider_for_brain()` 方法**

在 `impl LlmConfig` 中，`model_for_brain()` 之后新增：

```rust
/// 获取某个副脑应使用的 provider 名
pub fn provider_for_brain(&self, brain_name: &str) -> &str {
    self.llm
        .brain_providers
        .get(brain_name)
        .map_or(&self.llm.default_provider, |s| s.as_str())
}
```

**Step 3: 改造 `create_brain_client()`**

将 `create_brain_client()` 中的 `let provider_name = &self.llm.default_provider;` 替换为：

```rust
pub fn create_brain_client(&self, brain_name: &str) -> Result<Box<dyn LlmProvider>> {
    let model = self.model_for_brain(brain_name);
    let provider_name = self.provider_for_brain(brain_name);
    let api_key = self.resolve_api_key(provider_name)?;
    let (max_tokens, temperature) = self.params_for_brain(brain_name);

    let provider_config = self
        .llm
        .providers
        .get(provider_name)
        .ok_or_else(|| LlmError::ProviderNotFound(provider_name.to_string()))?;

    let client = OpenAiCompatClient::new(
        provider_config.api_base.clone(),
        api_key,
        model.to_string(),
        max_tokens,
        temperature,
    );

    Ok(Box::new(client))
}
```

**Step 4: 写测试**

在 `mod tests` 中新增：

```rust
#[test]
fn brain_providers_fallback_to_default() {
    let config = LlmConfig::default_config();
    // 没有配置 brain_providers，应 fallback 到 default_provider
    assert_eq!(config.provider_for_brain("main"), config.llm.default_provider);
    assert_eq!(config.provider_for_brain("unknown"), config.llm.default_provider);
}

#[test]
fn brain_providers_per_brain_routing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
[llm]
default_provider = "xiaomi"
default_model = "mimo-7b"

[llm.providers.xiaomi]
api_base = "https://xiaomi.example.com/v1"
api_key_env = "XIAOMI_API_KEY"

[llm.providers.deepseek]
api_base = "https://api.deepseek.com/v1"
api_key_env = "DEEPSEEK_API_KEY"

[llm.brain_providers]
main = "xiaomi"
eval = "deepseek"
"#,
    )
    .unwrap();

    let config = LlmConfig::load(&path).unwrap();
    assert_eq!(config.provider_for_brain("main"), "xiaomi");
    assert_eq!(config.provider_for_brain("eval"), "deepseek");
    // 未配置的脑走 default
    assert_eq!(config.provider_for_brain("memory"), "xiaomi");
    assert_eq!(config.provider_for_brain("unknown"), "xiaomi");
}

#[test]
fn load_config_without_brain_providers_is_backward_compatible() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    // 旧格式配置文件（没有 brain_providers）
    std::fs::write(
        &path,
        r#"
[llm]
default_provider = "zhipu"
default_model = "glm-4.7"

[llm.providers.zhipu]
api_base = "https://open.bigmodel.cn/api/paas/v4"
api_key_env = "ZHIPU_API_KEY"

[llm.brain_models]
reasoning = "glm-5.1"
"#,
    )
    .unwrap();

    let config = LlmConfig::load(&path).unwrap();
    // brain_providers 为空 HashMap，所有脑走 default
    assert!(config.llm.brain_providers.is_empty());
    assert_eq!(config.provider_for_brain("main"), "zhipu");
    assert_eq!(config.provider_for_brain("eval"), "zhipu");
}
```

**Step 5: 运行测试验证通过**

Run: `cargo test -p brain-llm -- config`
Expected: 所有测试 PASS

**Step 6: 更新现有测试**

`default_config_loads` 测试中确认 brain_providers 为空（新字段有 serde(default)）：

```rust
#[test]
fn default_config_loads() {
    let config = LlmConfig::default_config();
    assert_eq!(config.llm.default_provider, "zhipu");
    assert_eq!(config.llm.default_model, "glm-4.7");
    assert!(config.llm.brain_providers.is_empty());
    // ... 其余不变
}
```

**Step 7: Commit**

```bash
git add crates/brain-llm/src/config.rs
git commit -m "feat(brain-llm): 新增 brain_providers 配置，每脑可走不同厂商"
```

---

### Task 2: 配置层 — 更新 `default_config()` + `CONFIG_TEMPLATE`（默认厂商改为小米 MiMo）

**Files:**
- Modify: `crates/brain-llm/src/config.rs:184-266` (default_config)
- Modify: `crates/ai-brain-cli/src/init.rs:9-68` (CONFIG_TEMPLATE)
- Modify: `crates/ai-brain-cli/src/init.rs:103-124` (print_first_run_guide)

**Step 1: 更新 `LlmConfig::default_config()`**

将 `default_config()` 中的厂商和模型配置改为：

```rust
pub fn default_config() -> Self {
    let mut providers = HashMap::new();
    providers.insert(
        "xiaomi".into(),
        ProviderConfig {
            api_base: "https://xiaomi-llm.example.com/v1".into(),
            api_key_env: "XIAOMI_API_KEY".into(),
            api_key: None,
        },
    );
    providers.insert(
        "deepseek".into(),
        ProviderConfig {
            api_base: "https://api.deepseek.com/v1".into(),
            api_key_env: "DEEPSEEK_API_KEY".into(),
            api_key: None,
        },
    );
    providers.insert(
        "zhipu".into(),
        ProviderConfig {
            api_base: "https://open.bigmodel.cn/api/paas/v4".into(),
            api_key_env: "ZHIPU_API_KEY".into(),
            api_key: None,
        },
    );

    let mut brain_providers = HashMap::new();
    brain_providers.insert("main".into(), "xiaomi".into());
    brain_providers.insert("memory".into(), "xiaomi".into());
    brain_providers.insert("eval".into(), "deepseek".into());
    brain_providers.insert("evolver".into(), "xiaomi".into());

    let mut brain_models = HashMap::new();
    brain_models.insert("main".into(), "mimo-7b".into());
    brain_models.insert("sensory".into(), "mimo-7b".into());
    brain_models.insert("reasoning".into(), "mimo-7b".into());
    brain_models.insert("memory".into(), "mimo-7b".into());
    brain_models.insert("eval".into(), "deepseek-chat".into());
    brain_models.insert("evolver".into(), "mimo-7b".into());

    let mut brain_params = HashMap::new();
    // ... 保持原有 brain_params 不变（main/memory/eval/evolver/sensory/reasoning/compact）

    Self {
        llm: LlmSection {
            default_provider: "xiaomi".into(),
            default_model: "mimo-7b".into(),
            providers,
            brain_models,
            brain_params,
            brain_providers,
            defaults: LlmDefaults::default(),
        },
        brain: BrainSection::default(),
        hooks: None,
    }
}
```

**Step 2: 更新 `CONFIG_TEMPLATE`（init.rs）**

将 CONFIG_TEMPLATE 替换为包含小米 MiMo 默认配置的新模板：

```rust
const CONFIG_TEMPLATE: &str = r#"# AI Brain 配置文件
# 首次运行时自动生成，修改后重启生效

[llm]
default_provider = "xiaomi"
default_model = "mimo-7b"

# ── 厂商配置 ──────────────────────────────────────
[llm.providers.xiaomi]
api_base = "https://xiaomi-llm.example.com/v1"
api_key_env = "XIAOMI_API_KEY"

[llm.providers.deepseek]
api_base = "https://api.deepseek.com/v1"
api_key_env = "DEEPSEEK_API_KEY"

[llm.providers.zhipu]
api_base = "https://open.bigmodel.cn/api/paas/v4"
api_key_env = "ZHIPU_API_KEY"

# ── 每脑独立厂商（未配置的脑走 default_provider）────
[llm.brain_providers]
main = "xiaomi"
memory = "xiaomi"
eval = "deepseek"
evolver = "xiaomi"

[llm.brain_models]
main = "mimo-7b"
sensory = "mimo-7b"
reasoning = "mimo-7b"
memory = "mimo-7b"
eval = "deepseek-chat"
evolver = "mimo-7b"

[llm.defaults]
max_tokens = 4096
temperature = 0.7

[llm.brain_params.main]
max_tokens = 32768
temperature = 0.7

[llm.brain_params.memory]
max_tokens = 32768
temperature = 0.3

[llm.brain_params.eval]
max_tokens = 16384
temperature = 0.3

[llm.brain_params.evolver]
max_tokens = 16384
temperature = 0.3

[llm.brain_params.sensory]
max_tokens = 8192
temperature = 0.3

[llm.brain_params.reasoning]
max_tokens = 8192
temperature = 0.5

[llm.brain_params.compact]
max_tokens = 4096
temperature = 0.3
"#;
```

**Step 3: 更新 `print_first_run_guide()` 引导信息**

将引导信息中的 zhipu 引用改为 xiaomi：

```rust
pub fn print_first_run_guide() {
    eprintln!();
    eprintln!("========================================");
    eprintln!("  AI Brain 首次运行");
    eprintln!("========================================");
    eprintln!();
    eprintln!("  已生成默认配置文件:");
    eprintln!("  ~/.ai-brain/config.toml");
    eprintln!();
    eprintln!("  要使用 AI Brain，需要配置 LLM API Key:");
    eprintln!();
    eprintln!("    方式1（推荐）：设置环境变量");
    eprintln!("      export XIAOMI_API_KEY=your-key");
    eprintln!();
    eprintln!("    方式2：编辑配置文件");
    eprintln!("      vi ~/.ai-brain/config.toml");
    eprintln!("      在 [llm.providers.xiaomi] 下添加 api_key = \"your-key\"");
    eprintln!();
    eprintln!("  配置完成后重新运行 ai-brain 即可。");
    eprintln!("========================================");
    eprintln!();
}
```

**Step 4: 更新 `default_config_loads` 测试**

```rust
#[test]
fn default_config_loads() {
    let config = LlmConfig::default_config();
    assert_eq!(config.llm.default_provider, "xiaomi");
    assert_eq!(config.llm.default_model, "mimo-7b");
    assert!(config.llm.brain_providers.contains_key("main"));
    assert_eq!(config.provider_for_brain("eval"), "deepseek");
    assert_eq!(config.model_for_brain("main"), "mimo-7b");
    assert_eq!(config.model_for_brain("eval"), "deepseek-chat");
}
```

**Step 5: 更新 api key 测试**

`resolve_api_key_from_env` 和 `resolve_api_key_direct_config` 测试中改为使用 xiaomi provider：

```rust
#[test]
fn resolve_api_key_from_env() {
    let config = LlmConfig::default_config();
    std::env::set_var("XIAOMI_API_KEY", "test-key-123");
    let key = config.resolve_api_key("xiaomi").unwrap();
    assert_eq!(key, "test-key-123");
    std::env::remove_var("XIAOMI_API_KEY");
}

#[test]
fn resolve_api_key_direct_config() {
    let mut config = LlmConfig::default_config();
    if let Some(provider) = config.llm.providers.get_mut("xiaomi") {
        provider.api_key = Some("direct-key".into());
        provider.api_key_env = "NONEXISTENT_VAR".into();
    }
    let key = config.resolve_api_key("xiaomi").unwrap();
    assert_eq!(key, "direct-key");
}

#[test]
fn resolve_api_key_env_priority_over_direct() {
    let mut config = LlmConfig::default_config();
    if let Some(provider) = config.llm.providers.get_mut("xiaomi") {
        provider.api_key = Some("direct-key".into());
        provider.api_key_env = "TEST_PRIORITY_KEY".into();
    }
    std::env::set_var("TEST_PRIORITY_KEY", "env-key");
    let key = config.resolve_api_key("xiaomi").unwrap();
    assert_eq!(key, "env-key");
    std::env::remove_var("TEST_PRIORITY_KEY");
}
```

**Step 6: 运行测试验证**

Run: `cargo test -p brain-llm -- config && cargo test -p ai-brain-cli -- init`
Expected: 所有测试 PASS

**Step 7: Commit**

```bash
git add crates/brain-llm/src/config.rs crates/ai-brain-cli/src/init.rs
git commit -m "feat: 默认厂商改为小米 MiMo，更新配置模板和引导信息"
```

---

### Task 3: 公共工具 — `build_context_messages()` 函数

**Files:**
- Modify: `crates/brain-llm/src/provider.rs:177` (文件末尾追加)
- Test: `crates/brain-llm/src/provider.rs` (mod tests 新增)

**Step 1: 在 `provider.rs` 末尾追加公共工具函数**

```rust
/// 统一的上下文构建入口
///
/// 将稳定的 system 指令模板与变化的 user 数据分离，
/// 最大化 KV Cache 前缀命中率。
pub fn build_context_messages(system: &str, user: &str) -> Vec<ChatMessage> {
    let mut messages = Vec::with_capacity(2);
    if !system.is_empty() {
        messages.push(ChatMessage::system(system));
    }
    messages.push(ChatMessage::user(user));
    messages
}
```

**Step 2: 写测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_context_messages_both() {
        let msgs = build_context_messages("你是助手", "你好");
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, MessageRole::System);
        assert_eq!(msgs[1].role, MessageRole::User);
        assert_eq!(msgs[0].text_content(), "你是助手");
        assert_eq!(msgs[1].text_content(), "你好");
    }

    #[test]
    fn build_context_messages_empty_system() {
        let msgs = build_context_messages("", "你好");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, MessageRole::User);
    }

    #[test]
    fn build_context_messages_system_only_not_allowed() {
        // 不应该只传 system 不传 user，但函数不强制
        let msgs = build_context_messages("系统指令", "");
        assert_eq!(msgs.len(), 2); // system + user("") 都会生成
    }
}
```

**Step 3: 在 `lib.rs` 中 re-export**

确认 `pub use provider::{..., build_context_messages};` 已导出。

**Step 4: 运行测试**

Run: `cargo test -p brain-llm -- provider::tests`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/brain-llm/src/provider.rs crates/brain-llm/src/lib.rs
git commit -m "feat(brain-llm): 新增 build_context_messages 统一上下文构建"
```

---

### Task 4: `AnalysisLlm` trait 升级 — 支持 `analyze_structured`

**Files:**
- Modify: `crates/brain-memory/src/concentration.rs:24-33` (AnalysisLlm trait)
- Test: `crates/brain-memory/src/concentration.rs:376-386` (MockLlm impl)

**Step 1: 升级 `AnalysisLlm` trait**

将 `concentration.rs` 中的 `AnalysisLlm` trait 替换为：

```rust
/// LLM 分析接口（由调用方实现）
pub trait AnalysisLlm: Send + Sync {
    /// 结构化调用: 分离 system（稳定模板）和 user（变化数据）
    ///
    /// 将稳定的指令放入 system，变化的数据放入 user，
    /// 使得同一次浓缩运行中 system 部分保持不变，
    /// 最大化 KV Cache 前缀命中率。
    fn analyze_structured(
        &self,
        system: &str,
        user: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<String, String>> + Send + '_>,
    >;

    /// 向后兼容: 单条 prompt 全放 user
    fn analyze(
        &self,
        prompt: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<String, String>> + Send + '_>,
    > {
        self.analyze_structured("", prompt)
    }
}
```

**Step 2: 更新 `MockLlm` impl**

```rust
impl AnalysisLlm for MockLlm {
    fn analyze_structured(
        &self,
        _system: &str,
        _user: &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<String, String>> + Send + '_>,
    > {
        let response = self.response.clone();
        Box::pin(async move { Ok(response) })
    }
}
```

**Step 3: 验证测试通过（现有调用方走 `analyze()` 默认实现）**

Run: `cargo test -p brain-memory -- concentration`
Expected: 所有现有测试 PASS（`analyze()` 默认委托给 `analyze_structured("", prompt)`）

**Step 4: Commit**

```bash
git add crates/brain-memory/src/concentration.rs
git commit -m "feat(brain-memory): AnalysisLlm trait 升级支持 analyze_structured"
```

---

### Task 5: 浓缩引擎 prompt 拆分 — system/user 分离

**Files:**
- Modify: `crates/brain-memory/src/prompts.rs:782-996` (CONCENTRATION_STEP1-4 PROMPT)
- Modify: `crates/brain-memory/src/concentration.rs:152-281` (step1-4 调用)
- Test: `crates/brain-memory/src/prompts.rs` (测试更新)

**Step 1: 在 prompts.rs 中为每个 Step 新增 system prompt 常量**

在文件末尾（CONCENTRATION_STEP1-4 PROMPT 之后）新增拆分后的版本：

```rust
// ===========================================================================
// 金字塔浓缩 Prompt — 拆分版（system 稳定 + user 变化）
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
```

**Step 2: 新增拆分版 builder 函数**

在 `prompts.rs` 中追加（不删除旧的 builder，保持兼容）：

```rust
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
```

**Step 3: 改造 `concentration.rs` 中 step1-4 使用 `analyze_structured`**

将 `step1_l1_to_l2` 中的调用改为：

```rust
let (system, user) = prompts::build_concentration_step1_split(conversation_json, &existing_json);
let response = llm
    .analyze_structured(system, &user)
    .await
    .map_err(MemoryError::ConsolidationFailed)?;
```

同样的模式应用到 `step2_l2_to_l3`、`step3_l3_to_l4`、`step4_profile`。

**Step 4: 写测试**

在 `prompts.rs` 的 tests 中新增：

```rust
#[test]
fn concentration_step1_split_returns_nonempty() {
    let (sys, user) = build_concentration_step1_split(
        "[{\"role\":\"user\",\"content\":\"修复TUI\"}]",
        "[]",
    );
    assert!(!sys.is_empty());
    assert!(user.contains("修复TUI"));
}

#[test]
fn concentration_step4_split_returns_nonempty() {
    let (sys, user) = build_concentration_step4_split(
        "[{\"role\":\"user\"}]",
        "Rust开发者",
        "要测试\n别忘判空",
    );
    assert!(!sys.is_empty());
    assert!(user.contains("Rust开发者"));
}
```

**Step 5: 运行测试**

Run: `cargo test -p brain-memory`
Expected: 所有测试 PASS

**Step 6: Commit**

```bash
git add crates/brain-memory/src/concentration.rs crates/brain-memory/src/prompts.rs
git commit -m "feat(brain-memory): 浓缩引擎 prompt 拆分为 system/user，优化 KV Cache"
```

---

### Task 6: `AnalyzerLlm` 适配器升级 — 支持 `analyze_structured`

**Files:**
- Modify: `crates/ai-brain-cli/src/orchestrator.rs:92-150` (AnalyzerLlm struct + impl)

**Step 1: 为 `AnalyzerLlm` 新增 `complete_structured` 方法**

在 `impl AnalyzerLlm` 中（第 118 行附近）新增：

```rust
impl AnalyzerLlm {
    fn complete_structured(
        &self,
        system: &str,
        user: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
        let mut messages = vec![];
        if !system.is_empty() {
            messages.push(ChatMessage::system(system));
        }
        messages.push(ChatMessage::user(user));
        let request = ChatRequest {
            model: Some(self.model.clone()),
            messages,
            max_tokens: Some(self.max_tokens),
            temperature: Some(self.temperature),
            tools: None,
            tool_choice: None,
        };
        let client = self.client.clone();
        Box::pin(async move {
            client
                .complete(request)
                .await
                .map(|r| r.text())
                .map_err(|e| e.to_string())
        })
    }
}
```

**Step 2: 更新 `AnalysisLlm` trait impl**

```rust
impl brain_memory::concentration::AnalysisLlm for AnalyzerLlm {
    fn analyze_structured(
        &self,
        system: &str,
        user: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
        self.complete_structured(system, user)
    }
}
```

注意：`analyze()` 不再需要显式实现，trait 的默认实现会调用 `analyze_structured("", prompt)`。

**Step 3: 运行编译验证**

Run: `cargo build -p ai-brain-cli`
Expected: 编译成功

**Step 4: Commit**

```bash
git add crates/ai-brain-cli/src/orchestrator.rs
git commit -m "feat: AnalyzerLlm 适配器支持 analyze_structured 双参数调用"
```

---

### Task 7: `ThresholdCompressor` prompt 拆分

**Files:**
- Modify: `crates/brain-memory/src/threshold_compression.rs:90-244` (build + compress)

**Step 1: 提取稳定的压缩指令模板为常量**

在 `threshold_compression.rs` 中新增：

```rust
/// 压缩系统指令（稳定模板，KV Cache 可命中）
const COMPACT_SYSTEM_PROMPT: &str = r#"你是一个对话历史压缩专家。请将对话历史压缩成一个结构化的决策链路摘要。

## 压缩要求

**保留重点**：
1. **用户目标** — 用户最初想要什么，需求是否有变化
2. **用户反馈和指令** — 重点保留：
   - 用户的纠正："这个不对，应该要 xxxxx"
   - 用户的认可和改进建议："这个对了，但是可以 xxxx"
   - 用户的新需求/指令："很好，继续下一个需求，需求：xxxx"
   - 用户表达的偏好、标准、风格要求
3. **执行步骤** — 按时间顺序，做了哪些关键操作
4. **决策转折点** — 遇到了什么问题，如何调整方案的
5. **最终结果** — 得到了什么结论，完成了什么
6. **关键上下文** — 重要的文件路径、代码位置、配置信息

**丢弃内容**：
- 完整的代码输出、grep 结果、文件内容
- 中间过程的详细日志
- 重复的信息、确认性对话
- 工具调用的原始返回（只保留从中得出的结论）

## 输出格式

```
## 用户目标
[一句话描述用户的核心需求]

## 用户反馈和指令
- [纠正] "这个不对，应该要 xxxxx"
- [认可+改进] "这个对了，但是可以 xxxx"

## 执行过程
1. [第一步操作] → [结果/发现]

## 关键决策
- [遇到的问题] → [采取的解决方案] → [原因]

## 最终结论
[完成情况、核心成果、待办事项（如有）]

## 关键上下文
- 文件：[重要文件路径和修改内容]
- 配置：[关键配置项]
```"#;
```

**Step 2: 新增 `build_decision_chain_split()` 方法**

```rust
/// 构建拆分后的 prompt（system 稳定 + user 变化）
pub(crate) fn build_decision_chain_split(
    &self,
    messages: &[ConversationMessage],
) -> (&'static str, String) {
    let user = self.format_messages_for_prompt(messages);
    (COMPACT_SYSTEM_PROMPT, user)
}
```

**Step 3: 改造 `compress_context()` 使用拆分 prompt**

```rust
pub async fn compress_context(
    &self,
    messages: &[ConversationMessage],
    llm: &dyn LlmProvider,
) -> Result<CompressedContext, CompactionError> {
    let start = std::time::Instant::now();

    let min_messages = self.config.preserve_recent_turns * 2;
    if messages.len() < min_messages {
        return Err(CompactionError::InsufficientMessages);
    }

    let (old_messages, recent_messages) = self.split_messages(messages);

    let (system, user) = self.build_decision_chain_split(old_messages);

    let request = brain_llm::ChatRequest {
        model: None,
        messages: brain_llm::build_context_messages(system, &user),
        max_tokens: Some(self.config.max_summary_tokens),
        temperature: None,
        tools: None,
        tool_choice: None,
    };

    // ... 后续不变（timeout + response + Ok(...)）
}
```

**Step 4: 保留旧 `build_decision_chain_prompt()` 用于兼容测试**

旧方法不删除，只是不再被 `compress_context()` 调用。

**Step 5: 运行测试**

Run: `cargo test -p brain-memory -- threshold_compression`
Expected: PASS

**Step 6: Commit**

```bash
git add crates/brain-memory/src/threshold_compression.rs
git commit -m "feat(brain-memory): ThresholdCompressor prompt 拆分为 system/user"
```

---

### Task 8: Orchestrator 配置加载优化

**Files:**
- Modify: `crates/ai-brain-cli/src/orchestrator.rs:229-602` (Orchestrator::new)

**Step 1: 在 `Orchestrator::new()` 开头统一加载配置**

找到 `Orchestrator::new()` 中所有 `LlmConfig::load_default()` 调用点，替换为一次加载：

```rust
// 在 Orchestrator::new() 开头附近统一加载
let llm_config = LlmConfig::load_default()
    .expect("LLM 配置加载失败");
```

然后所有后续的 `LlmConfig::load_default()` 替换为 `&llm_config`：

- `create_sensory_llm()` → 传入 `&llm_config`
- `create_brain_client("eval")` → `llm_config.create_brain_client("eval")`
- `create_analyzer_llm_from_config()` → 传入 `&llm_config` 或直接用已创建的实例

**Step 2: 同步更新 `create_analyzer_llm()` 和 `create_analyzer_llm_from_config()`**

```rust
fn create_analyzer_llm_with_config(config: &LlmConfig) -> Option<AnalyzerLlm> {
    let client: Box<dyn brain_llm::LlmProvider> = config.create_brain_client("memory").ok()?;
    let model = config.model_for_brain("memory").to_string();
    let (mt, temp) = config.params_for_brain("memory");
    Some(AnalyzerLlm::new(Arc::from(client), model, mt, temp))
}
```

**Step 3: 编译验证**

Run: `cargo build -p ai-brain-cli`
Expected: 编译成功，无 warning

**Step 4: Commit**

```bash
git add crates/ai-brain-cli/src/orchestrator.rs
git commit -m "refactor: Orchestrator 配置加载从 6+ 次降为 1 次"
```

---

### Task 9: 全量编译 + 测试验证

**Step 1: 运行全量编译**

Run: `cargo build --workspace`
Expected: 编译成功

**Step 2: 运行全量测试**

Run: `cargo test --workspace --exclude brain-integration-tests`
Expected: 所有测试 PASS，0 failed

**Step 3: 运行 clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: 无 warning

**Step 4: 运行 fmt 检查**

Run: `cargo fmt --check`
Expected: 无格式问题

**Step 5: Commit**

```bash
git add -A
git commit -m "chore: 全量编译 + 测试 + clippy 验证通过"
```
