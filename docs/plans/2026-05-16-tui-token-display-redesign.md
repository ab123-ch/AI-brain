# TUI Token 显示重设计 — 实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 将 TUI 状态栏的 token 显示从"累计成本信息"改为"本轮累计 prompt token + LLM 调用次数"，每次 LLM 调用后实时刷新。

**Architecture:** 通过 ProgressEvent 新增 `TokenUpdate` 变体，tool_loop 每次调用 LLM 后推送 prompt tokens 累计值和调用次数到 TUI。StatusBar 精简字段，去掉成本估算相关代码。

**Tech Stack:** Rust, tokio mpsc channel, ratatui

---

**日期**: 2026-05-16
**状态**: 已批准

## 背景

当前 TUI 状态栏展示累计 token + 成本估算 + 缓存命中率，格式复杂且信息价值低。
用户更关心"本轮对话实际喂给模型多少数据"。

## 目标

将状态栏 token 显示简化为：**本轮对话累计 prompt token 数 + LLM 调用次数**，
每次 LLM 调用返回后实时刷新（包括 tool 调用后的后续调用）。

## 显示格式

```
gpt-4o | 上下文 45% | Tok: 12k(3次) | 第3轮 | 评估:开
```

## 数据流

```
tool_loop 每次 LLM 调用返回后
    ↓ 计算 prompt_tokens（API 返回的真实值，累加到 round_prompt_tokens）
    ↓ progress_sender.send(ProgressEvent::TokenUpdate { round_prompt_tokens, llm_calls })
    ↓
TUI App 接收 ProgressEvent::TokenUpdate
    ↓ 更新 status.round_prompt_tokens 和 status.round_llm_calls
    ↓
StatusBar.render()
    ↓ 显示 "Tok: 12k(3次)"
    ↓ 新轮次开始时（start_query）重置为 0
```

## 改动清单

### 1. brain-core/src/types.rs — ProgressEvent 新增变体
```rust
TokenUpdate {
    round_prompt_tokens: u64,  // 本轮累计 prompt tokens
    llm_calls: u32,            // 本轮 LLM 调用次数
},
```

### 2. brain-main/src/tool_loop.rs — 每次 LLM 调用后推送
- 在 tool_loop 主循环中，每次 LLM 返回结果后，累加 round_prompt_tokens
- 通过 progress_sender.send(ProgressEvent::TokenUpdate { ... }) 推送
- 所有 LLM 调用都触发推送（包括无 tool 的纯文本回复）

### 3. ai-brain-cli/src/tui/status.rs — StatusBar 字段替换
- 删除: cumulative_prompt_tokens, cumulative_completion_tokens, cumulative_cache_read_tokens, chars_saved_by_compaction, compaction_count
- 新增: round_prompt_tokens: u64, round_llm_calls: u32
- render(): 简化为 "Tok: Xk(N次)"
- 删除: short_tokens(), estimate_cost(), pricing_for_model() 等成本计算函数

### 4. ai-brain-cli/src/tui/app.rs — 事件处理
- start_query() 时重置 round_prompt_tokens = 0, round_llm_calls = 0
- 处理 ProgressEvent::TokenUpdate 更新对应字段

### 5. ai-brain-cli/src/orchestrator.rs — SystemStatus 精简
- SystemStatus 中删除累计 token 字段，新增 round_prompt_tokens / round_llm_calls
- status_structured() 简化

## 新轮次重置时机

app.rs 的 start_query() 中，发起查询前重置为 0。每轮对话独立统计。

## 不改动的部分

- brain-llm 中的 TokenUsage 结构体不变
- llm_usage_logger.rs 日志系统不变（独立于 TUI 显示）
- 上下文使用率 (context_usage) 计算不变

---

## 实施步骤

### Task 1: ProgressEvent 新增 TokenUpdate 变体

**Files:**
- Modify: `rust/crates/brain-core/src/types.rs:586` (Done 变体之前)

**Step 1: 在 ProgressEvent 枚举中 LlmRetry 和 Done 之间插入新变体**

在 `LlmRetry` 变体之后、`Done` 变体之前，插入：

```rust
    /// Token 使用量更新（每次 LLM 调用后推送）
    TokenUpdate {
        /// 本轮累计 prompt tokens
        round_prompt_tokens: u64,
        /// 本轮 LLM 调用次数
        llm_calls: u32,
    },
```

**Step 2: 验证编译**

Run: `cd rust && cargo check -p brain-core`
Expected: 编译通过

**Step 3: Commit**

```bash
git add rust/crates/brain-core/src/types.rs
git commit -m "feat: add ProgressEvent::TokenUpdate variant for real-time token display"
```

---

### Task 2: tool_loop 每次 LLM 调用后推送 TokenUpdate

**Files:**
- Modify: `rust/crates/brain-main/src/tool_loop.rs:253-258` (累计 prompt_tokens 之后)

**Step 1: 在 total_prompt_tokens 累加之后，立即推送 TokenUpdate**

在 `tool_loop.rs` 行 255 `total_prompt_tokens += last_prompt_tokens;` 之后插入：

```rust
        // 推送 TokenUpdate 到 TUI（每次 LLM 调用后实时更新）
        send_progress(
            progress_tx,
            ProgressEvent::TokenUpdate {
                round_prompt_tokens: total_prompt_tokens,
                llm_calls,
            },
        )
        .await;
```

**Step 2: 验证编译**

Run: `cd rust && cargo check -p brain-main`
Expected: 编译通过

**Step 3: Commit**

```bash
git add rust/crates/brain-main/src/tool_loop.rs
git commit -m "feat: push TokenUpdate after each LLM call in tool_loop"
```

---

### Task 3: StatusBar 字段替换 + 渲染简化

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/tui/status.rs` (完整重写)

**Step 1: 替换 StatusBar 结构体字段**

将 StatusBar 结构体改为：

```rust
#[derive(Default)]
pub struct StatusBar {
    /// 模型名
    pub model: String,
    /// 上下文使用率 (0.0~1.0)
    pub context_usage: f32,
    /// 当前轮次
    pub round: u32,
    /// 评估是否启用
    pub eval_enabled: bool,
    /// 是否正在处理（显示 spinner）
    pub busy: bool,
    /// 本轮累计 prompt tokens
    pub round_prompt_tokens: u64,
    /// 本轮 LLM 调用次数
    pub round_llm_calls: u32,
}
```

**Step 2: 删除 from_system_status 方法，替换 render 方法**

删除 `from_system_status` 方法（不再需要从 SystemStatus 构建）。

将 `render` 方法改为：

```rust
    pub fn render(&self, f: &mut Frame, area: Rect) {
        let ctx_pct_f = self.context_usage * 100.0;
        let ctx_display = if ctx_pct_f > 100.0 {
            format!("{:.0}\u{26a0}", ctx_pct_f)
        } else if ctx_pct_f < 1.0 {
            format!("{:.1}", ctx_pct_f)
        } else {
            format!("{}", ctx_pct_f as u32)
        };
        let ctx_color = if ctx_pct_f > 80.0 {
            Color::Red
        } else if ctx_pct_f > 50.0 {
            Color::Yellow
        } else {
            Color::Green
        };

        let eval_str = if self.eval_enabled {
            "\u{8bc4}\u{4f30}:\u{5f00}"
        } else {
            "\u{8bc4}\u{4f30}:\u{5173}"
        };
        let busy_indicator = if self.busy { " *" } else { "" };

        // Token 显示：本轮累计 prompt tokens + LLM 调用次数
        let tok_display = if self.round_prompt_tokens > 0 {
            let tok_str = short_tokens(self.round_prompt_tokens);
            format!("Tok: {tok_str}({}\u{6b21})", self.round_llm_calls)
        } else {
            String::new()
        };

        let text = format!(
            " {} | \u{4e0a}\u{4e0b}\u{6587} {}%{} | \u{7b2c}{}\u{8f6e} | {}{} ",
            self.model,
            ctx_display,
            if tok_display.is_empty() { String::new() } else { format!(" | {tok_display}") },
            self.round,
            eval_str,
            busy_indicator
        );

        let paragraph = Paragraph::new(text).style(
            Style::default()
                .fg(Color::White)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

        f.render_widget(paragraph, area);
    }
```

**Step 3: 保留 short_tokens，删除 short_chars / estimate_cost / pricing_for_model**

删除 `short_chars`、`estimate_cost`、`pricing_for_model` 三个函数。
保留 `short_tokens` 函数不变。

**Step 4: 验证编译**

Run: `cd rust && cargo check -p ai-brain-cli`
Expected: 编译失败（因为 app.rs 还在引用被删字段），这是预期的，Task 4 会修复。

---

### Task 4: app.rs 事件处理 — 处理 TokenUpdate + 重置逻辑

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/tui/app.rs`

**Step 1: 修改 start_query 方法**

将 `start_query` 中引用旧字段的代码替换。找到类似如下代码（约行 614-620）：

```rust
        let status = self.orch.status_structured();
        self.status.busy = true;
        self.status.round = status.query_count;
        self.status.context_usage = status.context_usage as f32;
        self.status.cumulative_prompt_tokens = status.cumulative_prompt_tokens;
        self.status.cumulative_completion_tokens = status.cumulative_completion_tokens;
```

替换为：

```rust
        let status = self.orch.status_structured();
        self.status.busy = true;
        self.status.round = status.query_count;
        self.status.context_usage = status.context_usage as f32;
        // 本轮 token 统计重置（新查询开始）
        self.status.round_prompt_tokens = 0;
        self.status.round_llm_calls = 0;
```

**Step 2: 在 process_progress_events 中处理 TokenUpdate**

在 `process_progress_events` 方法中，`self.output.handle_event(&event);` 之前插入：

```rust
                        // 处理 TokenUpdate：更新状态栏的实时 token 显示
                        if let ProgressEvent::TokenUpdate { round_prompt_tokens, llm_calls } = event {
                            self.status.round_prompt_tokens = round_prompt_tokens;
                            self.status.round_llm_calls = llm_calls;
                        }
                        self.output.handle_event(&event);
```

注意：需要在文件顶部确认 `use brain_core::types::ProgressEvent;` 已经导入。

**Step 3: 修改 flush_pending_result**

将 `flush_pending_result` 中的旧字段引用替换（约行 671-674）：

```rust
        let status = self.orch.status_structured();
        self.status.context_usage = status.context_usage as f32;
        self.status.cumulative_prompt_tokens = status.cumulative_prompt_tokens;
        self.status.cumulative_completion_tokens = status.cumulative_completion_tokens;
```

替换为：

```rust
        let status = self.orch.status_structured();
        self.status.context_usage = status.context_usage as f32;
```

**Step 4: 验证编译**

Run: `cd rust && cargo check -p ai-brain-cli`
Expected: 可能因 orchestrator.rs 的 SystemStatus 还未修改而失败，Task 5 会修复。

---

### Task 5: orchestrator.rs SystemStatus 精简

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`

**Step 1: 精简 SystemStatus 结构体**

将 SystemStatus 改为：

```rust
pub struct SystemStatus {
    pub model: String,
    pub query_count: u32,
    pub eval_enabled: bool,
    pub context_usage: f64,
}
```

删除字段：`cumulative_prompt_tokens`, `cumulative_completion_tokens`, `cumulative_cache_read_tokens`, `compaction_count`, `chars_saved_by_compaction`。

**Step 2: 简化 status_structured 方法**

将 `status_structured` 改为：

```rust
    pub fn status_structured(&self) -> SystemStatus {
        let context_usage = self
            .v2_brain
            .try_lock()
            .ok()
            .and_then(|g| g.as_ref().map(|b| b.context_usage()))
            .unwrap_or(0.0);
        SystemStatus {
            model: self.model_name.clone(),
            query_count: self.query_count.load(std::sync::atomic::Ordering::Relaxed),
            eval_enabled: true,
            context_usage,
        }
    }
```

**Step 3: 验证全 workspace 编译**

Run: `cd rust && cargo check --workspace`
Expected: 编译通过（所有引用旧字段的地方已在 Task 3/4 中清理）

**Step 4: Commit**

```bash
git add rust/crates/ai-brain-cli/src/orchestrator.rs rust/crates/ai-brain-cli/src/tui/status.rs rust/crates/ai-brain-cli/src/tui/app.rs
git commit -m "feat: simplify StatusBar to show round prompt tokens + LLM call count"
```

---

### Task 6: 运行全量测试 + clippy 检查

**Step 1: 运行测试**

Run: `cd rust && cargo test --workspace`
Expected: 所有测试通过

**Step 2: 运行 clippy**

Run: `cd rust && cargo clippy --workspace --all-targets -- -D warnings`
Expected: 无警告

**Step 3: 格式化检查**

Run: `cd rust && cargo fmt --check`
Expected: 无格式问题

**Step 4: 最终 Commit（如有格式修正）**

```bash
cd rust && cargo fmt
git add -A
git commit -m "chore: fmt after token display redesign"
```
