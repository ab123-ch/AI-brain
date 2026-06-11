# 每脑独立 LLM Provider 路由设计

> 日期: 2026-06-11
> 状态: 已确认

## 背景与目标

### 当前问题

1. **单 Provider 架构**: `create_brain_client()` 硬编码使用 `default_provider`，所有脑共享同一个厂商的 api_base
2. **上下文构造不统一**: 记忆脑用 `AnalysisLlm(string→string)` 单条 prompt，压缩器也是单条 user 消息，无法命中 KV Cache 前缀匹配
3. **配置重复加载**: `LlmConfig::load_default()` 在 `Orchestrator::new()` 中被调用 6+ 次

### 目标

1. 每个副脑可以独立配置不同的 LLM 厂商（如主脑用小米 MiMo、记忆脑用 DeepSeek、进化脑用本地 Ollama）
2. 所有脑的上下文构造统一为 `system(稳定模板) + user(变化数据)` 结构，最大化 KV Cache 前缀命中率
3. 零破坏性变更，向后兼容

### 默认配置

- **默认厂商**: 小米 MiMo 系列
- **保留厂商**: DeepSeek v4、智谱（智谱仅保留，不使用）
- **默认模型**: `mimo-7b` (小米 MiMo)

## 设计

### Part 1: 配置层 — `brain_providers` HashMap

#### 1.1 TOML 配置结构

```toml
[llm]
default_provider = "xiaomi"
default_model = "mimo-7b"

[llm.providers.xiaomi]
api_base = "https://xiaomi-llm.example.com/v1"   # 小米 MiMo API 地址
api_key_env = "XIAOMI_API_KEY"

[llm.providers.deepseek]
api_base = "https://api.deepseek.com/v1"
api_key_env = "DEEPSEEK_API_KEY"

[llm.providers.zhipu]
api_base = "https://open.bigmodel.cn/api/paas/v4"
api_key_env = "ZHIPU_API_KEY"

# 新增: 每脑独立厂商，未配置的脑走 default_provider
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

[llm.defaults]
max_tokens = 4096
temperature = 0.7
```

#### 1.2 Rust 结构体改动

`brain-llm/src/config.rs` — `LlmSection` 新增字段:

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
    /// ★ 新增: 每脑独立厂商映射
    #[serde(default)]
    pub brain_providers: HashMap<String, String>,
    #[serde(default)]
    pub defaults: LlmDefaults,
}
```

#### 1.3 `create_brain_client()` 改造

```rust
pub fn create_brain_client(&self, brain_name: &str) -> Result<Box<dyn LlmProvider>> {
    let model = self.model_for_brain(brain_name);
    // ★ 改动: 先查 brain_providers，fallback default_provider
    let provider_name = self.llm
        .brain_providers
        .get(brain_name)
        .map_or(&self.llm.default_provider, |s| s.as_str());
    let api_key = self.resolve_api_key(provider_name)?;
    let (max_tokens, temperature) = self.params_for_brain(brain_name);
    let provider_config = self.llm.providers.get(provider_name)
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

**向后兼容**: `brain_providers` 有 `#[serde(default)]`，不配就是空 HashMap，所有脑走 `default_provider`。

#### 1.4 新增 `provider_for_brain()` 公共方法

```rust
/// 获取某个副脑应使用的 provider 名
pub fn provider_for_brain(&self, brain_name: &str) -> &str {
    self.llm
        .brain_providers
        .get(brain_name)
        .map_or(&self.llm.default_provider, |s| s.as_str())
}
```

### Part 2: 上下文构造统一化

#### 2.1 问题: 各脑上下文构造方式不一致

| 脑 | 当前构造方式 | KV Cache 命中 |
|---|---|---|
| 主脑 | system prompt + Vec<ChatMessage> 历史 | system 不变 → 可命中 |
| 评估脑 | system prompt + 单条 user | system 不变 → 可命中 |
| 记忆脑 | 单条 user prompt（全量重建） | 完全无法命中 |
| 压缩器 | 单条 user prompt（全量重建） | 完全无法命中 |

#### 2.2 `AnalysisLlm` trait 升级

`brain-memory/src/concentration.rs`:

```rust
/// LLM 分析接口（由调用方实现）
pub trait AnalysisLlm: Send + Sync {
    /// 结构化调用: 分离 system（稳定模板）和 user（变化数据）
    fn analyze_structured(
        &self,
        system: &str,
        user: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>>;

    /// 向后兼容: 单条 prompt 全放 user
    fn analyze(
        &self,
        prompt: &str,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>> {
        self.analyze_structured("", prompt)
    }
}
```

#### 2.3 浓缩引擎 prompt 拆分

每个 Step 的 prompt 拆分为 system（稳定指令模板）+ user（变化数据）:

```
system (稳定, 约 500-800 token):
  - 角色定义
  - 输出格式说明（JSON schema）
  - 分析原则和步骤

user (变化):
  - 实际数据（conversation_json / summary_json / experience_json）
```

4 个 Step 在同一次浓缩运行中 system 完全不变 → KV Cache 连续命中。

#### 2.4 `AnalyzerLlm` 适配器升级

`ai-brain-cli/src/orchestrator.rs`:

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
        // ... 调用 client.complete(request)
    }
}
```

#### 2.5 公共工具函数

`brain-llm/src/provider.rs`（或 `brain-core`）:

```rust
/// 统一的上下文构建入口
/// 确保 system → user 结构一致，最大化 KV Cache 前缀命中
pub fn build_context_messages(system: &str, user: &str) -> Vec<ChatMessage> {
    let mut messages = Vec::with_capacity(2);
    if !system.is_empty() {
        messages.push(ChatMessage::system(system));
    }
    messages.push(ChatMessage::user(user));
    messages
}
```

### Part 3: 压缩逻辑统一化

#### 3.1 `ThresholdCompressor` 拆分 prompt

`brain-memory/src/threshold_compression.rs`:

```rust
// 改造前
messages: vec![ChatMessage::user(prompt)]  // 完整 prompt = 指令 + 数据

// 改造后
messages: vec![
    ChatMessage::system(COMPACT_SYSTEM_PROMPT),  // 稳定的压缩指令模板
    ChatMessage::user(formatted_messages),        // 变化的对话数据
]
```

`COMPACT_SYSTEM_PROMPT` 包含压缩角色定义、输出格式、保留/丢弃规则（约 500 token），不再每次重建。

### Part 4: 配置模板与初始化优化

#### 4.1 `init.rs` 配置模板更新

`ai-brain-cli/src/init.rs` 的 `CONFIG_TEMPLATE` 更新为包含:
- 小米 MiMo 默认厂商
- DeepSeek v4 厂商配置
- 智谱厂商配置（保留）
- `brain_providers` 示例（注释状态）

#### 4.2 `LlmConfig::default_config()` 更新

默认配置改为:
- `default_provider = "xiaomi"`
- `default_model = "mimo-7b"`
- providers 包含 xiaomi + deepseek + zhipu
- brain_models 全部使用 mimo-7b（eval 除外使用 deepseek-chat）

#### 4.3 Orchestrator 配置加载优化

`Orchestrator::new()` 中 `LlmConfig::load_default()` 从 6+ 次降为 1 次:

```rust
let llm_config = LlmConfig::load_default()
    .expect("配置加载失败");
// 后续所有脑共用这一个 config 实例
```

### 统一后的各脑 KV Cache 策略

| 组件 | 消息结构 | system 稳定性 | KV Cache 命中 |
|---|---|---|---|
| 主脑 | system(思维框架+环境) + history | system 每次微调 | 部分命中 |
| 评估脑 | system(评估框架) + user(评估数据) | system 不变 | 命中 |
| 记忆脑 4 步 | system(分析模板) + user(数据) | 同一次浓缩不变 | 4 步连续命中 |
| 压缩器 | system(压缩模板) + user(对话) | system 不变 | 命中 |

## 涉及文件清单

| 文件 | 改动 |
|---|---|
| `brain-llm/src/config.rs` | `brain_providers` 字段 + `provider_for_brain()` + `create_brain_client()` 改路由 + `default_config()` 更新 |
| `brain-llm/src/provider.rs` | `build_context_messages()` 工具函数 |
| `brain-memory/src/concentration.rs` | `AnalysisLlm` trait 升级 + 浓缩引擎 prompt 拆分 |
| `brain-memory/src/prompts.rs` | 各 Step prompt 拆分为 system/user 模板 |
| `brain-memory/src/threshold_compression.rs` | 压缩器 prompt 拆分 |
| `ai-brain-cli/src/orchestrator.rs` | `AnalyzerLlm` 适配器升级 + 配置加载优化 |
| `ai-brain-cli/src/init.rs` | 配置模板更新 |
| 各 crate tests | 测试补全 |

## 兼容性

- **TOML 配置**: `brain_providers` 有 `#[serde(default)]`，不配就等于之前的行为
- **`AnalysisLlm` trait**: `analyze()` 有默认实现（委托 `analyze_structured("", prompt)`），现有调用方无需改动
- **`ThresholdCompressor`**: prompt 拆分是内部实现，公共 API 不变
