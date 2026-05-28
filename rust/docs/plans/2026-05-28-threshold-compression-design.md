# 阈值触发智能压缩模块设计

**日期**: 2026-05-28  
**状态**: 设计完成  
**模块**: brain-main / threshold_compression

---

## 1. 背景和问题

### 当前问题

当上下文使用率超过 80%（`context_danger_threshold`）时，系统直接执行强制截断 — 丢弃除最近 20 条消息外的所有内容：

```rust
// brain-main/src/main_brain.rs（当前实现）
if self.history.is_context_full(thresholds.context_danger_threshold) {
    let truncated = self.history.truncate_to_recent(20);  // 粗暴截断
}
```

这导致：
- **上下文断裂** — 用户之前的需求、讨论、决策全部丢失
- **重复劳动** — 用户需要重新解释之前讨论过的内容
- **体验差** — 助手"失忆"，无法延续之前的对话

### 现有压缩机制

| 阈值 | 当前行为 | 问题 |
|------|---------|------|
| 60% | 后台异步 LLM 压缩 | 可能来不及，80% 时仍在压缩中 |
| 80% | 强制截断到最近 20 条 | 粗暴，信息丢失严重 |

**缺失**：60% 到 80% 之间缺乏一个"智能摘要"缓冲层。

---

## 2. 设计目标

### 核心目标

**在 80% 阈值时，用智能摘要替代粗暴截断**，保留对话的决策链路。

### 设计约束

1. **同步阻塞** — 等摘要完成再继续，确保上下文完整
2. **保留决策链路** — 用户目标 → 执行步骤 → 最终结果
3. **保留用户反馈** — 纠正、认可、新需求、偏好标准
4. **错误处理** — 直接报错，让上层决定如何处理
5. **整合到 brain-main** — 复用现有压缩基础设施

---

## 3. 模块设计

### 3.1 新增文件

```
brain-main/src/
├── threshold_compression.rs    # 新增：阈值触发智能压缩
├── main_brain.rs               # 修改：集成新模块
└── conversation.rs             # 修改：新增上下文重建方法
```

### 3.2 核心结构

```rust
// threshold_compression.rs

/// 阈值压缩配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdCompactionConfig {
    /// 保留最近 N 轮对话（默认 4）
    pub preserve_recent_turns: usize,
    
    /// 摘要最大 token 数（默认 2048）
    pub max_summary_tokens: u32,
    
    /// 决策链路保留强度（0.0-1.0，默认 0.8）
    /// 越高越保留详细的决策过程
    pub decision_chain_weight: f64,
    
    /// 压缩超时时间（毫秒，默认 30000）
    pub timeout_ms: u64,
}

impl Default for ThresholdCompactionConfig {
    fn default() -> Self {
        Self {
            preserve_recent_turns: 4,
            max_summary_tokens: 2048,
            decision_chain_weight: 0.8,
            timeout_ms: 30_000,
        }
    }
}

/// 压缩后的上下文
#[derive(Debug, Clone)]
pub struct CompressedContext {
    /// 决策链路摘要
    pub decision_summary: String,
    
    /// 保留的最近消息
    pub recent_messages: Vec<Message>,
    
    /// 压缩元数据
    pub metadata: CompressionMetadata,
}

/// 压缩元数据
#[derive(Debug, Clone)]
pub struct CompressionMetadata {
    /// 原始消息总数
    pub original_count: usize,
    
    /// 压缩的消息数
    pub compressed_count: usize,
    
    /// 保留的消息数
    pub preserved_count: usize,
    
    /// 压缩时间（毫秒）
    pub duration_ms: u64,
}

/// 压缩错误类型
#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    #[error("LLM 调用失败: {0}")]
    LlmError(String),
    
    #[error("消息分割失败: 消息数不足")]
    InsufficientMessages,
    
    #[error("压缩超时（超过 {0}ms）")]
    Timeout(u64),
}

/// 阈值压缩器
pub struct ThresholdCompressor {
    config: ThresholdCompactionConfig,
}
```

### 3.3 核心方法

```rust
impl ThresholdCompressor {
    pub fn new(config: ThresholdCompactionConfig) -> Self {
        Self { config }
    }
    
    /// 压缩上下文（同步阻塞，直接报错）
    pub async fn compress_context(
        &self,
        messages: &[Message],
        llm: &dyn LlmClient,
    ) -> Result<CompressedContext, CompactionError> {
        let start = std::time::Instant::now();
        
        // 1. 校验消息数量
        if messages.len() < self.config.preserve_recent_turns * 2 {
            return Err(CompactionError::InsufficientMessages);
        }
        
        // 2. 分割消息：旧消息 vs 最近 N 轮
        let (old_messages, recent_messages) = self.split_messages(messages);
        
        // 3. 构建决策链路保留 prompt
        let prompt = self.build_decision_chain_prompt(&old_messages);
        
        // 4. 调用 LLM（带超时）
        let summary = tokio::time::timeout(
            Duration::from_millis(self.config.timeout_ms),
            llm.complete(prompt, self.config.max_summary_tokens),
        )
        .await
        .map_err(|_| CompactionError::Timeout(self.config.timeout_ms))?
        .map_err(|e| CompactionError::LlmError(e.to_string()))?;
        
        // 5. 构建结果
        Ok(CompressedContext {
            decision_summary: summary,
            recent_messages: recent_messages.to_vec(),
            metadata: CompressionMetadata {
                original_count: messages.len(),
                compressed_count: old_messages.len(),
                preserved_count: recent_messages.len(),
                duration_ms: start.elapsed().as_millis() as u64,
            },
        })
    }
    
    /// 分割消息
    fn split_messages<'a>(&self, messages: &'a [Message]) -> (&'a [Message], &'a [Message]) {
        let split_point = messages.len().saturating_sub(self.config.preserve_recent_turns * 2);
        (&messages[..split_point], &messages[split_point..])
    }
    
    /// 构建决策链路保留 prompt
    fn build_decision_chain_prompt(&self, messages: &[Message]) -> String {
        // 见 3.4 节
    }
}
```

---

## 4. Prompt 设计

### 4.1 决策链路保留 Prompt

```rust
fn build_decision_chain_prompt(&self, messages: &[Message]) -> String {
    let weight_desc = match self.config.decision_chain_weight {
        w if w >= 0.8 => "详细保留每一步决策过程和关键转折点",
        w if w >= 0.5 => "保留主要决策节点和关键结论",
        _ => "只保留最终决策和核心结论",
    };
    
    format!(
        r#"你是一个对话历史压缩专家。请将以下对话历史压缩成一个结构化的决策链路摘要。

## 压缩要求

**保留重点**（决策链路保留强度：{weight}）：
1. **用户目标** — 用户最初想要什么，需求是否有变化
2. **用户反馈和指令** — ⭐ 重点保留
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

请严格按照以下格式输出：

```
## 用户目标
[一句话描述用户的核心需求]

## 用户反馈和指令
- [纠正] "这个不对，应该要 xxxxx"
- [认可+改进] "这个对了，但是可以 xxxx"
- [新需求] "继续下一个需求：xxxx"
- [偏好/标准] "我喜欢 xxx 风格"、"要求 xxx 标准"

## 执行过程
1. [第一步操作] → [结果/发现]
2. [第二步操作] → [结果/发现]
3. ...（按时间顺序）

## 关键决策
- [遇到的问题] → [采取的解决方案] → [原因]

## 最终结论
[完成情况、核心成果、待办事项（如有）]

## 关键上下文
- 文件：[重要文件路径和修改内容]
- 配置：[关键配置项]
- 其他：[需要记住的重要信息]
```

## 对话历史

{formatted_messages}

## 开始压缩

请提取决策链路，生成结构化摘要："#,
        weight = weight_desc,
        formatted_messages = self.format_messages_for_prompt(messages),
    )
}

/// 格式化消息用于 prompt
fn format_messages_for_prompt(&self, messages: &[Message]) -> String {
    messages.iter().enumerate().map(|(i, msg)| {
        let role = match msg.role {
            Role::User => "用户",
            Role::Assistant => "助手",
            Role::Tool => "工具",
        };
        let content = if msg.content.len() > 500 {
            format!(
                "{}...[中间省略 {} 字符]...{}",
                &msg.content[..200],
                msg.content.len() - 400,
                &msg.content[msg.content.len()-200..]
            )
        } else {
            msg.content.clone()
        };
        format!("【{}】{}\n{}", role, i + 1, content)
    }).collect::<Vec<_>>().join("\n---\n")
}
```

---

## 5. 系统集成

### 5.1 main_brain.rs 修改

```rust
// main_brain.rs

use crate::threshold_compression::{ThresholdCompressor, ThresholdCompactionConfig};

pub struct MainBrain {
    // ... 现有字段
    
    // 新增：阈值压缩器
    threshold_compressor: ThresholdCompressor,
}

impl MainBrain {
    pub fn new(config: BrainConfig) -> Self {
        // ... 现有初始化
        
        // 初始化阈值压缩器
        let threshold_config = ThresholdCompactionConfig {
            preserve_recent_turns: config.compaction.preserve_recent_turns,
            max_summary_tokens: config.compaction.max_tokens,
            decision_chain_weight: 0.8,
            timeout_ms: 30_000,
        };
        let threshold_compressor = ThresholdCompressor::new(threshold_config);
        
        Self {
            // ...
            threshold_compressor,
        }
    }
    
    /// 处理用户输入的核心流程
    pub async fn process_input(&mut self, input: String) -> Result<Response> {
        // Step 0: 保存用户消息
        // Step 1: 应用后台预压缩结果
        
        // Step 2: 检查上下文使用率
        if self.history.is_context_full(thresholds.context_danger_threshold) {
            log::warn!(
                "上下文使用率超过阈值: {:.1}% >= {:.1}%, 开始智能压缩...",
                self.history.context_usage() * 100.0,
                thresholds.context_danger_threshold * 100.0
            );
            
            // 调用智能压缩（替换原来的强制截断）
            match self.threshold_compressor
                .compress_context(self.history.messages(), self.llm.as_ref())
                .await
            {
                Ok(compressed) => {
                    // 成功：用压缩结果重建上下文
                    self.history.clear_and_rebuild_from_compressed(
                        &compressed.decision_summary,
                        &compressed.recent_messages,
                    );
                    
                    log::info!(
                        "阈值压缩完成：压缩了 {} 条消息，保留了 {} 条最近消息，耗时 {}ms",
                        compressed.metadata.compressed_count,
                        compressed.metadata.preserved_count,
                        compressed.metadata.duration_ms,
                    );
                }
                Err(e) => {
                    // 失败：记录错误，返回错误给用户
                    log::error!("阈值压缩失败: {}", e);
                    return Err(AppError::ContextCompactionFailed(e.to_string()));
                }
            }
        }
        
        // Step 3: 构建 messages
        // Step 4: 跑 tool_loop
        // Step 5: 保存响应
        // Step 6: 后台预压缩
    }
}
```

### 5.2 conversation.rs 新增方法

```rust
// conversation.rs

impl ConversationHistory {
    /// 从压缩结果重建上下文
    pub fn clear_and_rebuild_from_compressed(
        &mut self,
        summary: &str,
        recent_messages: &[Message],
    ) {
        // 1. 清空现有消息
        self.messages.clear();
        
        // 2. 添加压缩摘要作为系统消息
        let summary_message = Message {
            role: Role::System,
            content: format!(
                r#"以下是之前对话的决策链路摘要：

{summary}

请基于这个摘要继续对话，不要重复已经完成的工作。"#,
                summary = summary
            ),
            timestamp: chrono::Utc::now(),
        };
        self.messages.push(summary_message);
        
        // 3. 追加最近的消息
        self.messages.extend(recent_messages.iter().cloned());
        
        // 4. 重置 token 追踪
        self.tracked_prompt_tokens = 0;
    }
}
```

### 5.3 错误类型定义

```rust
// brain-main/src/errors.rs

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    // ... 现有错误类型
    
    #[error("上下文压缩失败: {0}")]
    ContextCompactionFailed(String),
}
```

### 5.4 压缩流程对比

```
原有流程（粗暴截断）：
┌─────────────────────────────────────────┐
│ 80% 阈值触发                            │
│   ↓                                     │
│ truncate_to_recent(20)  ← 丢失所有历史  │
│   ↓                                     │
│ 继续对话（上下文断裂）                   │
└─────────────────────────────────────────┘

新流程（智能压缩）：
┌─────────────────────────────────────────┐
│ 80% 阈值触发                            │
│   ↓                                     │
│ threshold_compressor.compress_context() │
│   ├── 分割消息（旧 vs 最近N轮）         │
│   ├── 构建决策链路 prompt               │
│   ├── LLM 生成摘要（同步阻塞）          │
│   └── 返回 CompressedContext            │
│   ↓                                     │
│ clear_and_rebuild_from_compressed()     │
│   ↓                                     │
│ 继续对话（上下文连续）                   │
└─────────────────────────────────────────┘
```

---

## 6. 测试策略

### 6.1 单元测试

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    struct MockLlmClient {
        response: String,
        should_fail: bool,
    }
    
    #[async_trait]
    impl LlmClient for MockLlmClient {
        async fn complete(&self, prompt: String, max_tokens: u32) -> Result<String> {
            if self.should_fail {
                Err(anyhow::anyhow!("Mock LLM error"))
            } else {
                Ok(self.response.clone())
            }
        }
    }
    
    #[tokio::test]
    async fn test_compress_context_success() {
        // 测试正常压缩流程
    }
    
    #[tokio::test]
    async fn test_compress_context_insufficient_messages() {
        // 测试消息数不足的情况
    }
    
    #[tokio::test]
    async fn test_compress_context_llm_failure() {
        // 测试 LLM 调用失败
    }
    
    #[test]
    fn test_split_messages() {
        // 测试消息分割逻辑
    }
    
    #[test]
    fn test_build_decision_chain_prompt() {
        // 测试 prompt 生成
    }
}
```

### 6.2 集成测试

```rust
#[tokio::test]
async fn test_threshold_compression_integration() {
    // 端到端测试：从 80% 阈值触发到压缩完成
}
```

### 6.3 测试覆盖重点

| 测试类型 | 覆盖点 |
|---------|--------|
| 单元测试 | 分割逻辑、Prompt 生成、错误类型转换、配置默认值 |
| 集成测试 | 端到端压缩流程、与 MainBrain 集成、上下文重建 |
| 边界测试 | 消息数刚好等于 preserve_recent_turns*2、空消息列表 |
| 错误测试 | LLM 调用失败、超时、无效响应格式 |

---

## 7. 性能考虑

### 7.1 性能指标

```rust
#[derive(Debug, Clone, Serialize)]
pub struct PerformanceMetrics {
    /// 压缩耗时（毫秒）
    pub compression_duration_ms: u64,
    
    /// LLM 调用耗时（毫秒）
    pub llm_call_duration_ms: u64,
    
    /// 压缩前 token 数
    pub tokens_before: usize,
    
    /// 压缩后 token 数
    pub tokens_after: usize,
    
    /// 压缩率（0.0-1.0，越低压缩越多）
    pub compression_ratio: f64,
    
    /// 摘要字符数
    pub summary_length: usize,
}
```

### 7.2 性能预算

| 指标 | 预算值 | 说明 |
|------|--------|------|
| 压缩耗时 | ≤ 30 秒 | 超时则报错 |
| 压缩率 | ≥ 10% | 至少保留 10% 内容 |
| 摘要长度 | ≤ 10K 字符 | 避免摘要过长 |

### 7.3 日志记录

```rust
log::info!(
    r#"阈值压缩成功:
  - 压缩耗时: {}ms（LLM: {}ms）
  - 消息数: {} → {}（压缩了 {} 条）
  - Token: {} → {}（压缩率: {:.1}%）
  - 摘要长度: {} 字符"#,
    metrics.compression_duration_ms,
    metrics.llm_call_duration_ms,
    compressed.metadata.original_count,
    compressed.metadata.preserved_count,
    compressed.metadata.compressed_count,
    metrics.tokens_before,
    metrics.tokens_after,
    metrics.compression_ratio * 100.0,
    metrics.summary_length,
);
```

---

## 8. 配置管理

### 8.1 配置结构

```rust
// brain-core/src/config.rs

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    // 现有配置
    pub preserve_recent_turns: usize,
    pub tool_result_compress_threshold: usize,
    pub max_tokens: u32,
    
    // 新增：阈值压缩配置
    pub threshold_compaction: ThresholdCompactionConfig,
}
```

### 8.2 默认配置

```rust
impl Default for ThresholdCompactionConfig {
    fn default() -> Self {
        Self {
            preserve_recent_turns: 4,
            max_summary_tokens: 2048,
            decision_chain_weight: 0.8,
            timeout_ms: 30_000,
        }
    }
}
```

---

## 9. 未来扩展

### 可能的增强方向

1. **异步压缩选项** — 对于非关键对话，可以异步压缩，当前轮次先用截断
2. **压缩质量评估** — 压缩后评估摘要质量，质量太低可以重试
3. **增量压缩** — 支持在已有摘要基础上增量更新
4. **多模态压缩** — 支持图片、代码块等非文本内容的压缩
5. **用户偏好学习** — 根据用户反馈调整压缩策略

---

## 10. 总结

### 核心改动

| 文件 | 改动类型 | 说明 |
|------|---------|------|
| `brain-main/src/threshold_compression.rs` | 新增 | 阈值触发智能压缩模块 |
| `brain-main/src/main_brain.rs` | 修改 | 集成新模块，替换强制截断 |
| `brain-main/src/conversation.rs` | 修改 | 新增上下文重建方法 |
| `brain-main/src/errors.rs` | 修改 | 新增错误类型 |
| `brain-core/src/config.rs` | 修改 | 新增配置项 |

### 设计亮点

1. **智能压缩** — 保留决策链路，不是粗暴截断
2. **用户反馈优先** — 重点保留用户的纠正、认可、新需求
3. **同步阻塞** — 确保上下文完整
4. **错误处理** — 直接报错，让上层决定
5. **性能监控** — 收集压缩耗时、压缩率等指标
6. **可配置** — 保留强度、超时时间等都可配置
