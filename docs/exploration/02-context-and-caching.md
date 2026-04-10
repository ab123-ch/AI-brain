# 上下文管理与缓存机制探索报告

> 探索日期: 2026-04-09

## 一、消息发送机制

### 1.1 全量历史发送

每次 LLM 调用都发送**完整的 session.messages**，不是增量。

**核心代码**：`crates/runtime/src/conversation.rs:312-315`

```rust
let request = ApiRequest {
    system_prompt: self.system_prompt.clone(),
    messages: self.session.messages.clone(),  // 全量克隆
};
```

**CLI 层组装**：`crates/rusty-claude-cli/src/main.rs:4143-4153`

```rust
let message_request = MessageRequest {
    model: self.model.clone(),
    max_tokens: max_tokens_for_model(&self.model),
    messages: convert_messages(&request.messages),
    system: (!request.system_prompt.is_empty()).then(|| request.system_prompt.join("\n\n")),
    tools: self.enable_tools.then(|| filter_tool_specs(&self.tool_registry, self.allowed_tools.as_ref())),
    tool_choice: self.enable_tools.then_some(ToolChoice::Auto),
    stream: true,
};
```

### 1.2 Tool Definition 每次全量发送

每次 API 调用都传完整的 `tools` 列表（30+ 个 ToolDefinition）。但由于服务端自动前缀缓存，tools 部分的 token 在后续调用中从 cache 读取。

---

## 二、缓存机制

### 2.1 服务端自动缓存（Prompt Caching Scope）

**配置方式**：通过 `anthropic-beta` 请求头

```rust
// crates/telemetry/src/lib.rs:69-71
betas: vec![
    "claude-code-20250219",
    "prompt-caching-scope-2026-01-05",  // 自动前缀缓存
],
```

**特点**：
- 不需要在消息块上标注 `cache_control`
- 服务端根据请求前缀（system + tools + messages）的稳定性自动管理缓存
- 只要前面的部分不变，自动命中缓存
- 中间任何修改导致后面的缓存全部失效

**缓存断裂检测**：`crates/api/src/prompt_cache.rs:314-382`

```rust
// 连续两次请求间 cache_read_input_tokens 下降 > 2000 → 记录为 cache break
// 通过比对四个哈希（model/system/tools/messages）定位原因
```

### 2.2 本地 Completion Cache

**完整实现**：`crates/api/src/prompt_cache.rs`

| 属性 | 值 |
|------|-----|
| 缓存 key | 对整个 MessageRequest 做 FNV-1a 哈希 |
| 存储路径 | `~/.claude/cache/prompt-cache/{session-id}/completions/{hash}.json` |
| TTL | 默认 30 秒 |
| 命中条件 | 请求完全一致 + 未过期 |
| 写入时机 | API 调用成功后自动写入 |

---

## 三、上下文压缩（Auto-Compaction）

### 3.1 触发条件

`crates/runtime/src/compact.rs` + `conversation.rs:507-530`

```
条件: cumulative_input_tokens > 100,000
时机: 每次 run_turn 结束时检查
```

### 3.2 压缩方式

```
保留: 最近 4 条消息（preserve_recent_messages）
压缩: 其余全部 → 一个 XML 格式的摘要
```

### 3.3 摘要内容

```xml
<summary>
Conversation summary:
- Scope: N earlier messages compacted (user=X, assistant=Y, tool=Z).
- Tools mentioned: bash, read_file, grep_search.
- Recent user requests:
  - 用户最近的请求（最多3条，截断160字符）
- Pending work:
  - 含 todo/next/pending 关键词的内容
- Key files referenced: src/main.rs, lib/types.rs.
- Current work: 最近一条非空文本消息
- Key timeline:
  - user: 消息摘要
  - assistant: 消息摘要
  - tool: 工具调用摘要
</summary>
```

### 3.4 二次压缩

已有摘要 + 新压缩内容合并：
```
[Previously compacted context] + [Newly compacted context]
```

### 3.5 与新架构记忆脑压缩的对比

| 维度 | Claude Code compact | 记忆脑压缩（新设计） |
|------|-------------------|-------------------|
| 粒度 | 整段历史一刀切 | 逐条工具调用智能判断 |
| 判断依据 | token 数量阈值 > 100k | LLM 判断每条内容价值 |
| 压缩方式 | 全部变成一个摘要 | 高价值保留原文，低价值才压缩 |
| 原文存储 | 不保存，丢失 | 存入记忆脑，可召回 |
| 召回能力 | 无 | 通过引用 ID 随时召回 |
| 触发时机 | run_turn 结束后 | 步骤之间（不在步骤内） |

**结论：runtime::compact 不可复用，记忆脑压缩需要全新实现。**

---

## 四、工具结果大小控制

### 4.1 已有限制

| 工具 | 限制 | 代码位置 |
|------|------|---------|
| Glob | 最多 100 个文件名 | `file_ops.rs:248-259` |
| Grep | 默认 250 条匹配 | `file_ops.rs:416` |
| Read | 默认 2000 行 | 通过参数控制 offset/limit |
| Bash | 无截断 | 完整返回 stdout/stderr |
| Write/Edit | 无截断 | 完整返回操作结果 |

### 4.2 没有全局工具输出截断机制

工具结果以完整字符串直接存入 session，无二次处理。

---

## 五、Token 估算

**最粗糙的估算**：`crates/runtime/src/compact.rs:392-404`

```rust
fn estimate_message_tokens(message: &ConversationMessage) -> usize {
    message.blocks.iter().map(|block| match block {
        ContentBlock::Text { text } => text.len() / 4 + 1,
        ContentBlock::ToolUse { name, input, .. } => (name.len() + input.len()) / 4 + 1,
        ContentBlock::ToolResult { tool_name, output, .. } => (tool_name.len() + output.len()) / 4 + 1,
    }).sum()
}
```

字符数 / 4，仅用于 compaction 触发判断。实际 token 计数来自 API 响应的 Usage 字段。

**没有 token 预算机制。**

---

## 六、System Prompt 构建

### 6.1 多段式构建器

`crates/runtime/src/prompt.rs` — `SystemPromptBuilder`

```
1. intro_section       — 角色介绍
2. output_style        — 输出风格（可选）
3. system_section      — 系统规则（6条行为约束）
4. doing_tasks_section — 任务执行准则
5. actions_section     — 操作安全
6. DYNAMIC_BOUNDARY    — 静态/动态分界
7. environment_section — 模型/日期/平台
8. project_context     — cwd/git_status/git_diff
9. instruction_files   — CLAUDE.md 内容
10. runtime_config     — 运行时配置
11. append_sections    — 自定义追加
```

### 6.2 指令文件发现

`discover_instruction_files()` 从 cwd 向上遍历目录树：
- `CLAUDE.md`
- `CLAUDE.local.md`
- `.claw/CLAUDE.md`
- `.claw/instructions.md`

每个文件最大 4000 字符，总预算 12000 字符。

### 6.3 与新架构的差距

- 单一构建器 → 新架构需要多副脑差异化 prompt（不可直接复用）
- 硬编码 sections → 需要动态 section（工具列表/技能库/记忆/任务状态）
- 部分可复用：文件发现逻辑、环境信息收集
