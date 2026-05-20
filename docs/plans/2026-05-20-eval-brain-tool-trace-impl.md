# 评估脑操作轨迹可见性 实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 让评估脑能看到主脑的所有工具调用轨迹，并将评估脑从固定两轮重构为 tool_loop 循环架构。

**Architecture:** 在 `prompts.rs` 新增 `format_tool_trace()` 将所有 TurnRecord 格式化为摘要注入评估脑 prompt；在 `eval_brain.rs` 新增 `eval_tool_loop()` 替代固定两轮逻辑，统一有/无文件变更两条路径。orchestrator 和 brain-core 零改动。

**Tech Stack:** Rust, tokio async, brain-llm (ChatMessage/ChatRequest/ContentBlock/LlmProvider), brain-core (TurnRecord/ToolCallRecord/TurnRole)

**Design doc:** `docs/plans/2026-05-20-eval-brain-tool-trace-design.md`

---

### Task 1: 新增 `format_tool_trace()` 函数

**Files:**
- Modify: `rust/crates/brain-eval/src/prompts.rs`
- Test: `rust/crates/brain-eval/src/prompts.rs` (tests module)

**Step 1: 写失败测试**

在 `prompts.rs` 的 `tests` 模块末尾添加：

```rust
use brain_core::types::{ToolCallRecord, TurnRecord, TurnRole};

#[test]
fn format_tool_trace_shows_all_tool_types() {
    let turns = vec![
        TurnRecord {
            role: TurnRole::ToolCall,
            content: String::new(),
            tool_call: Some(ToolCallRecord {
                tool_name: "bash".into(),
                input: serde_json::json!({"command": "curl -s wttr.in/Ningbo"}),
                output: "Weather report: Ningbo\nPartly Cloudy +24°C\n...".into(),
                duration_ms: 1249,
                is_error: false,
            }),
            timestamp: String::new(),
        },
        TurnRecord {
            role: TurnRole::ToolCall,
            content: String::new(),
            tool_call: Some(ToolCallRecord {
                tool_name: "bash".into(),
                input: serde_json::json!({"command": "rm -rf /"}),
                output: "安全拒绝: dangerous command".into(),
                duration_ms: 0,
                is_error: true,
            }),
            timestamp: String::new(),
        },
        TurnRecord {
            role: TurnRole::Assistant,
            content: "中间推理文本".into(),
            tool_call: None,
            timestamp: String::new(),
        },
        TurnRecord {
            role: TurnRole::ToolCall,
            content: String::new(),
            tool_call: Some(ToolCallRecord {
                tool_name: "edit_file".into(),
                input: serde_json::json!({
                    "file_path": "src/main.rs",
                    "old_string": "fn old()",
                    "new_string": "fn new() {}"
                }),
                output: "OK".into(),
                duration_ms: 10,
                is_error: false,
            }),
            timestamp: String::new(),
        },
    ];

    let result = format_tool_trace(&turns);

    // 验证包含所有工具调用
    assert!(result.contains("主脑操作轨迹"));
    assert!(result.contains("[bash]"));
    assert!(result.contains("curl -s wttr.in/Ningbo"));
    assert!(result.contains("成功(1249ms)"));
    assert!(result.contains("失败(0ms)"));
    assert!(result.contains("安全拒绝"));
    assert!(result.contains("[edit_file]"));
    assert!(result.contains("src/main.rs"));
    // Assistant 角色的记录不应出现
    assert!(!result.contains("中间推理文本"));
}

#[test]
fn format_tool_trace_empty_returns_empty() {
    let result = format_tool_trace(&[]);
    assert!(result.is_empty());
}

#[test]
fn format_tool_trace_only_assistant_returns_empty() {
    let turns = vec![TurnRecord {
        role: TurnRole::Assistant,
        content: "回答".into(),
        tool_call: None,
        timestamp: String::new(),
    }];
    let result = format_tool_trace(&turns);
    assert!(result.is_empty());
}

#[test]
fn format_tool_trace_output_truncated() {
    let long_output: String = "x".repeat(500);
    let turns = vec![TurnRecord {
        role: TurnRole::ToolCall,
        content: String::new(),
        tool_call: Some(ToolCallRecord {
            tool_name: "bash".into(),
            input: serde_json::json!({"command": "cat big_file.txt"}),
            output: long_output,
            duration_ms: 100,
            is_error: false,
        }),
        timestamp: String::new(),
    }];
    let result = format_tool_trace(&turns);
    // 输出应被截断到 300 字符
    assert!(result.contains("xxx")); // 有内容
    assert!(!result.contains(&"x".repeat(400))); // 但不超过300
}

#[test]
fn format_tool_trace_edit_file_shows_old_new() {
    let turns = vec![TurnRecord {
        role: TurnRole::ToolCall,
        content: String::new(),
        tool_call: Some(ToolCallRecord {
            tool_name: "edit_file".into(),
            input: serde_json::json!({
                "file_path": "src/lib.rs",
                "old_string": "fn before()",
                "new_string": "fn after() {}"
            }),
            output: "OK".into(),
            duration_ms: 5,
            is_error: false,
        }),
        timestamp: String::new(),
    }];
    let result = format_tool_trace(&turns);
    assert!(result.contains("替换前: fn before()"));
    assert!(result.contains("替换后: fn after() {}"));
}
```

**Step 2: 运行测试确认失败**

Run: `cd rust && cargo test -p brain-eval -- format_tool_trace 2>&1 | tail -20`
Expected: 编译失败，`format_tool_trace` 未定义

**Step 3: 实现 `format_tool_trace()`**

在 `prompts.rs` 中（`format_file_changes` 函数之后）添加：

```rust
/// 将主脑操作轨迹格式化为 prompt 文本（所有工具调用，不仅文件变更）
///
/// 格式：
/// ```text
/// ## 主脑操作轨迹
/// 1. [bash] curl -s wttr.in/Ningbo → 成功(1249ms)
///    输入: {"command":"curl -s wttr.in/Ningbo"}
///    输出摘要: Weather report: Ningbo...
/// 2. [edit_file] src/main.rs → 成功(10ms)
///    替换前: fn old()
///    替换后: fn new() {}
/// ```
pub fn format_tool_trace(turns: &[TurnRecord]) -> String {
    use brain_core::types::TurnRole;

    const MAX_OUTPUT_CHARS: usize = 300;

    let tool_turns: Vec<_> = turns
        .iter()
        .filter(|t| t.role == TurnRole::ToolCall && t.tool_call.is_some())
        .collect();

    if tool_turns.is_empty() {
        return String::new();
    }

    let mut s = String::from("## 主脑操作轨迹\n\n");
    for (i, t) in tool_turns.iter().enumerate() {
        let tc = t.tool_call.as_ref().unwrap();
        let status = if tc.is_error { "失败" } else { "成功" };
        let _ = writeln!(
            s,
            "{}. [{}] → {}({}ms)",
            i + 1,
            tc.tool_name,
            status,
            tc.duration_ms
        );
        let input_str = serde_json::to_string(&tc.input).unwrap_or_default();
        let _ = writeln!(s, "   输入: {input_str}");

        if tc.tool_name == "edit_file" {
            // edit_file 显示替换前后内容
            if let Some(old) = tc.input.get("old_string").and_then(|v| v.as_str()) {
                let truncated: String = old.chars().take(500).collect();
                let _ = writeln!(s, "   替换前: {truncated}");
            }
            if let Some(new) = tc.input.get("new_string").and_then(|v| v.as_str()) {
                let truncated: String = new.chars().take(500).collect();
                let _ = writeln!(s, "   替换后: {truncated}");
            }
        } else if tc.tool_name == "write_file" {
            if let Some(content) = tc.input.get("content").and_then(|v| v.as_str()) {
                let truncated: String = content.chars().take(500).collect();
                let _ = writeln!(s, "   内容: {truncated}");
            }
        } else {
            // 其他工具：输出摘要
            if tc.is_error {
                let _ = writeln!(s, "   错误: {}", tc.output);
            } else {
                let truncated: String = tc.output.chars().take(MAX_OUTPUT_CHARS).collect();
                let ellipsis = if tc.output.chars().count() > MAX_OUTPUT_CHARS { "..." } else { "" };
                let _ = writeln!(s, "   输出摘要: {truncated}{ellipsis}");
            }
        }
        s.push('\n');
    }
    s
}
```

**Step 4: 运行测试确认通过**

Run: `cd rust && cargo test -p brain-eval -- format_tool_trace 2>&1 | tail -20`
Expected: 5 tests passed

**Step 5: 提交**

```bash
git add rust/crates/brain-eval/src/prompts.rs
git commit -m "feat(eval): 新增 format_tool_trace() 格式化主脑所有工具调用轨迹"
```

---

### Task 2: 修改 `build_evaluation_user_prompt()` 使用 turns 替代 file_changes

**Files:**
- Modify: `rust/crates/brain-eval/src/prompts.rs`

**Step 1: 写失败测试**

在 `prompts.rs` 的 tests 模块中添加：

```rust
#[test]
fn user_prompt_with_turns_shows_tool_trace() {
    let turns = vec![TurnRecord {
        role: TurnRole::ToolCall,
        content: String::new(),
        tool_call: Some(ToolCallRecord {
            tool_name: "bash".into(),
            input: serde_json::json!({"command": "curl wttr.in"}),
            output: "weather data".into(),
            duration_ms: 500,
            is_error: false,
        }),
        timestamp: String::new(),
    }];

    let prompt = build_evaluation_user_prompt(
        "查天气",
        "今天24度",
        &[],
        &UserProfile::default(),
        &[],
        &turns,
    );
    assert!(prompt.contains("主脑操作轨迹"));
    assert!(prompt.contains("[bash]"));
}

#[test]
fn user_prompt_with_turns_no_trace_when_empty() {
    let prompt = build_evaluation_user_prompt(
        "闲聊",
        "你好",
        &[],
        &UserProfile::default(),
        &[],
        &[],
    );
    assert!(!prompt.contains("主脑操作轨迹"));
}
```

**Step 2: 运行测试确认失败**

Run: `cd rust && cargo test -p brain-eval -- user_prompt_with_turns 2>&1 | tail -20`
Expected: 编译失败，`build_evaluation_user_prompt` 参数类型不匹配

**Step 3: 修改 `build_evaluation_user_prompt()` 签名和实现**

将函数签名从：
```rust
pub fn build_evaluation_user_prompt(
    user_input: &str,
    ai_output: &str,
    pitfalls: &[PitfallRecord],
    user_profile: &UserProfile,
    rules: &[EvolutionRule],
    file_changes: &[FileChange],
) -> String
```

改为：
```rust
pub fn build_evaluation_user_prompt(
    user_input: &str,
    ai_output: &str,
    pitfalls: &[PitfallRecord],
    user_profile: &UserProfile,
    rules: &[EvolutionRule],
    turns: &[TurnRecord],
) -> String
```

在函数体中，将原来 `file_changes` 相关的代码：
```rust
let changes_text = format_file_changes(file_changes);
if !changes_text.is_empty() {
    prompt.push_str(&changes_text);
}
```

替换为：
```rust
let trace_text = format_tool_trace(turns);
if !trace_text.is_empty() {
    prompt.push_str(&trace_text);
}
```

同时更新文件顶部的 import，新增：
```rust
use brain_core::types::TurnRecord;
```

**注意**：`FileChange` / `FileChangeType` 的 import 不再需要，可以删除。`format_file_changes` 函数可以保留（不被调用但保留代码无害），或者删除。

**Step 4: 修复 eval_brain.rs 中调用 build_evaluation_user_prompt 的编译错误**

eval_brain.rs 中有两处调用需要改：

1. `evaluate()` 方法中（约第357行）：
   - 删除 `let file_changes = extractor::extract_file_changes(turns);`
   - 将 `&file_changes` 改为 `turns`

2. `llm_evaluate_fallback()` 方法中（约第574行）：
   - 将 `&[]` 保持为 `&[]`（空 turns slice，类型从 `&[FileChange]` 变为 `&[TurnRecord]`）

**Step 5: 修复所有现有测试**

现有测试中 `build_evaluation_user_prompt` 的调用需要把最后一个参数 `&[]` 或 `&changes` 改为 turns 类型。

对于 `user_prompt_with_file_changes` 测试：
```rust
// 旧代码用了 FileChange，改为用 TurnRecord
#[test]
fn user_prompt_with_file_changes_shows_trace() {
    let turns = vec![TurnRecord {
        role: TurnRole::ToolCall,
        content: String::new(),
        tool_call: Some(ToolCallRecord {
            tool_name: "edit_file".into(),
            input: serde_json::json!({
                "file_path": "src/main.rs",
                "old_string": "fn old()",
                "new_string": "fn new() {}"
            }),
            output: "OK".into(),
            duration_ms: 10,
            is_error: false,
        }),
        timestamp: String::new(),
    }];
    let prompt = build_evaluation_user_prompt(
        "改代码", "已修改", &[], &UserProfile::default(), &[], &turns,
    );
    assert!(prompt.contains("主脑操作轨迹"));
    assert!(prompt.contains("src/main.rs"));
    assert!(prompt.contains("fn new()"));
}
```

**Step 6: 运行全部测试确认通过**

Run: `cd rust && cargo test -p brain-eval 2>&1 | tail -30`
Expected: all tests passed

**Step 7: 提交**

```bash
git add rust/crates/brain-eval/src/prompts.rs rust/crates/brain-eval/src/eval_brain.rs
git commit -m "refactor(eval): build_evaluation_user_prompt 使用 turns 替代 file_changes"
```

---

### Task 3: 实现 `eval_tool_loop()` 函数

**Files:**
- Modify: `rust/crates/brain-eval/src/eval_brain.rs`

**Step 1: 写失败测试**

在 `eval_brain.rs` 的 tests 模块中添加：

```rust
#[tokio::test]
async fn eval_tool_loop_calls_multiple_rounds() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let call_count = Arc::new(AtomicUsize::new(0));
    let count_clone = call_count.clone();

    struct MultiRoundLlm {
        call_count: Arc<AtomicUsize>,
    }

    impl LlmProvider for MultiRoundLlm {
        fn model(&self) -> &'static str { "mock" }

        fn complete(
            &self,
            _request: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = brain_llm::Result<ChatResponse>> + Send + '_>> {
            let count = self.call_count.clone();
            Box::pin(async move {
                let n = count.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    // Round 1: 调用 read_file
                    Ok(ChatResponse {
                        content: vec![
                            ContentBlock::text("需要验证"),
                            ContentBlock::ToolUse {
                                id: "tu_1".into(),
                                name: "read_file".into(),
                                input: serde_json::json!({"file_path": "/tmp/test.rs"}),
                            },
                        ],
                        model: "mock".into(),
                        usage: brain_llm::TokenUsage::default(),
                        finish_reason: Some(brain_llm::FinishReason::ToolUse),
                    })
                } else if n == 1 {
                    // Round 2: 调用 Skill
                    Ok(ChatResponse {
                        content: vec![
                            ContentBlock::text("加载审查规则"),
                            ContentBlock::ToolUse {
                                id: "tu_2".into(),
                                name: "Skill".into(),
                                input: serde_json::json!({"command": "code-verification"}),
                            },
                        ],
                        model: "mock".into(),
                        usage: brain_llm::TokenUsage::default(),
                        finish_reason: Some(brain_llm::FinishReason::ToolUse),
                    })
                } else {
                    // Round 3: 最终评估
                    Ok(ChatResponse {
                        content: vec![ContentBlock::text("评估结果-正常")],
                        model: "mock".into(),
                        usage: brain_llm::TokenUsage::default(),
                        finish_reason: Some(brain_llm::FinishReason::EndTurn),
                    })
                }
            })
        }
    }

    let llm = Arc::new(MultiRoundLlm { call_count: count_clone });
    let executor = Arc::new(brain_core::tool_executor::StubToolExecutor::new());
    let brain = EvalBrain::with_verification(llm, executor);

    let turns = vec![make_edit_turn("/tmp/test.rs", "old", "new")];
    let result = brain
        .evaluate(
            "改代码",
            "已修改",
            &turns,
            &[],
            &UserProfile::default(),
            &[],
            &[],
        )
        .await
        .unwrap();

    assert!(result.passed);
    assert_eq!(call_count.load(Ordering::SeqCst), 3); // 确认三轮 LLM
}

#[tokio::test]
async fn eval_tool_loop_max_rounds_forced_result() {
    // LLM 每轮都调工具，达到最大轮次后强制出结果
    use std::sync::atomic::{AtomicUsize, Ordering};
    let call_count = Arc::new(AtomicUsize::new(0));
    let count_clone = call_count.clone();

    struct AlwaysToolLlm {
        call_count: Arc<AtomicUsize>,
    }

    impl LlmProvider for AlwaysToolLlm {
        fn model(&self) -> &'static str { "mock" }

        fn complete(
            &self,
            _request: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = brain_llm::Result<ChatResponse>> + Send + '_>> {
            let count = self.call_count.clone();
            Box::pin(async move {
                let n = count.fetch_add(1, Ordering::SeqCst);
                if n < 10 {
                    // 每轮都调工具
                    Ok(ChatResponse {
                        content: vec![
                            ContentBlock::text("继续检查"),
                            ContentBlock::ToolUse {
                                id: format!("tu_{n}"),
                                name: "read_file".into(),
                                input: serde_json::json!({"file_path": format!("/tmp/{n}")}),
                            },
                        ],
                        model: "mock".into(),
                        usage: brain_llm::TokenUsage::default(),
                        finish_reason: Some(brain_llm::FinishReason::ToolUse),
                    })
                } else {
                    // 强制轮：出结果
                    Ok(ChatResponse {
                        content: vec![ContentBlock::text("评估结果-正常")],
                        model: "mock".into(),
                        usage: brain_llm::TokenUsage::default(),
                        finish_reason: Some(brain_llm::FinishReason::EndTurn),
                    })
                }
            })
        }
    }

    let llm = Arc::new(AlwaysToolLlm { call_count: count_clone });
    let executor = Arc::new(brain_core::tool_executor::StubToolExecutor::new());
    let brain = EvalBrain::with_verification(llm, executor);

    let turns = vec![make_edit_turn("/tmp/test.rs", "old", "new")];
    let result = brain
        .evaluate(
            "改代码",
            "已修改",
            &turns,
            &[],
            &UserProfile::default(),
            &[],
            &[],
        )
        .await
        .unwrap();

    assert!(result.passed);
    // 10轮工具 + 1轮强制结果 = 11
    assert_eq!(call_count.load(Ordering::SeqCst), 11);
}
```

**Step 2: 运行测试确认失败**

Run: `cd rust && cargo test -p brain-eval -- eval_tool_loop 2>&1 | tail -20`
Expected: 编译失败或测试失败（当前固定两轮逻辑无法支持多轮）

**Step 3: 实现 `eval_tool_loop()` 并重构 `evaluate()`**

在 `eval_brain.rs` 中（`EvalBrain` impl 块内）新增：

```rust
/// 评估脑专用 tool_loop — 与主脑架构一致，但只允许只读工具
///
/// LLM 自主决定调多少轮工具，直到不再调用工具或达到最大轮次。
async fn eval_tool_loop(
    llm: &dyn LlmProvider,
    tool_executor: &dyn ToolExecutor,
    messages: Vec<ChatMessage>,
    tools: Vec<ToolDefinition>,
    skill_registry: &SkillRegistry,
    max_rounds: usize,
) -> Result<String> {
    let mut messages = messages;
    for round in 0..max_rounds {
        let request = ChatRequest {
            model: None,
            messages: messages.clone(),
            max_tokens: Some(2048),
            temperature: Some(0.1),
            tools: Some(tools.clone()),
            tool_choice: Some(ToolChoice::Auto),
        };

        let response = llm
            .complete(request)
            .await
            .map_err(|e| EvalError::LlmError(e.to_string()))?;

        if !response.has_tool_calls() {
            return Ok(response.text());
        }

        tracing::info!("评估脑 tool_loop 第{round}轮: LLM 调用了 {} 个工具", response.tool_calls().len());
        messages.push(ChatMessage::assistant_blocks(response.content.clone()));

        for tool_block in response.tool_calls() {
            if let ContentBlock::ToolUse { id, name, input } = tool_block {
                let output = Self::execute_eval_tool(name, input, id, tool_executor, skill_registry);
                messages.push(output);
            }
        }
    }

    // 超过最大轮次 → 强制无工具出结果
    tracing::warn!("评估脑 tool_loop 达到最大轮次 {max_rounds}，强制出结果");
    let request = ChatRequest {
        model: None,
        messages,
        max_tokens: Some(2048),
        temperature: Some(0.1),
        tools: None,
        tool_choice: None,
    };
    let response = llm
        .complete(request)
        .await
        .map_err(|e| EvalError::LlmError(e.to_string()))?;
    Ok(response.text())
}

/// 执行单个评估脑工具调用（白名单校验 + Skill/bash 特殊处理）
fn execute_eval_tool(
    name: &str,
    input: &serde_json::Value,
    id: &str,
    tool_executor: &dyn ToolExecutor,
    skill_registry: &SkillRegistry,
) -> ChatMessage {
    // 安全校验：只允许只读工具
    if !is_read_only_tool(name) {
        tracing::warn!("评估脑工具安全拒绝: {name}");
        return ChatMessage::tool_result(
            id,
            format!("工具 {name} 不可用：评估脑只允许只读工具"),
            true,
        );
    }

    // Skill tool 特殊处理
    if name == "Skill" {
        let skill_name = input.get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let content = skill_registry
            .get_skill_content(skill_name)
            .map_or_else(|| format!("Skill '{skill_name}' 不存在"), ToString::to_string);
        return ChatMessage::tool_result(id, content, false);
    }

    // bash 命令白名单检查
    if name == "bash" {
        let cmd = input.get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !is_read_only_bash_command(cmd) {
            tracing::warn!("评估脑 bash 命令安全拒绝: {cmd}");
            return ChatMessage::tool_result(
                id,
                format!("命令 '{cmd}' 不在只读白名单中"),
                true,
            );
        }
    }

    // 通用工具执行
    let tool_call = ToolCall {
        tool_name: name.clone(),
        input: input.clone(),
        validated: false,
        validation_id: None,
    };
    // 注意：这里需要同步执行，但 tool_executor.execute 是 async
    // 实际实现中需要在 eval_tool_loop 中用 .await
    // 这里简化为返回一个占位，实际代码需要在 eval_tool_loop 中处理
    todo!("需要在 eval_tool_loop 中 async 处理")
}
```

**重要**：由于 `execute_eval_tool` 需要 async 调用 `tool_executor.execute()`，实际实现应将工具执行逻辑内联在 `eval_tool_loop` 的循环中（和现有代码类似），而非拆成独立同步方法。上面的伪代码展示的是逻辑，实际代码参考现有 `evaluate()` 中第 398-473 行的工具执行逻辑，内联到循环体中。

**重构 `evaluate()` 方法**：

删除固定两轮逻辑，改为：

```rust
pub async fn evaluate(
    &self,
    user_input: &str,
    ai_output: &str,
    turns: &[TurnRecord],
    pitfalls: &[PitfallRecord],
    user_profile: &UserProfile,
    rules: &[EvolutionRule],
    eval_requirements: &[EvalRequirement],
) -> Result<EvalResult> {
    if user_input.trim().is_empty() || ai_output.trim().is_empty() {
        return Err(EvalError::InvalidInput(
            "user_input and ai_output must not be empty".into(),
        ));
    }

    // 发送评估开始事件
    if let Some(tx) = &self.progress_tx {
        let _ = tx.try_send(ProgressEvent::EvaluationStart);
        let ai_output_preview: String = ai_output.chars().take(200).collect();
        let _ = tx.try_send(ProgressEvent::EvaluationContext {
            user_input: user_input.to_string(),
            ai_output_preview,
            pitfalls_count: pitfalls.len(),
            rules_count: rules.len(),
            file_changes_count: extractor::extract_file_changes(turns).len(),
        });
    }

    let system_prompt = prompts::build_evaluation_system_prompt(
        eval_requirements,
        &self.skill_registry,
        true,
    );
    let user_prompt = prompts::build_evaluation_user_prompt(
        user_input,
        ai_output,
        pitfalls,
        user_profile,
        rules,
        turns,
    );

    let messages = vec![
        ChatMessage::system(&system_prompt),
        ChatMessage::user(&user_prompt),
    ];

    let final_text = match &self.tool_executor {
        Some(executor) => {
            let tools = build_read_only_tool_definitions();
            Self::eval_tool_loop(
                self.llm.as_ref(),
                executor.as_ref(),
                messages,
                tools,
                &self.skill_registry,
                10,
            )
            .await?
        }
        None => {
            // 无 tool_executor → 单次 LLM 调用（降级）
            let request = ChatRequest {
                model: None,
                messages,
                max_tokens: Some(2048),
                temperature: Some(0.1),
                tools: None,
                tool_choice: None,
            };
            let response = self.llm.complete(request).await
                .map_err(|e| EvalError::LlmError(e.to_string()))?;
            response.text()
        }
    };

    if final_text.trim().is_empty() {
        self.emit_result(true, "评估结果-正常");
        return Ok(EvalResult::passed());
    }

    let passed = !final_text.contains("存在问题");
    self.emit_result(passed, &final_text);
    Ok(EvalResult {
        passed,
        feedback: final_text.trim().to_string(),
    })
}
```

**删除 `llm_evaluate_fallback()` 方法**（其逻辑已合并到 evaluate 的 None 分支中）。

**Step 4: 运行全部测试**

Run: `cd rust && cargo test -p brain-eval 2>&1 | tail -30`
Expected: all tests passed（包括新的 eval_tool_loop 测试）

**Step 5: 运行 workspace 全部测试**

Run: `cd rust && cargo test --workspace 2>&1 | tail -30`
Expected: all tests passed（orchestrator 不需要改，因为 evaluate 的签名没变）

**Step 6: 提交**

```bash
git add rust/crates/brain-eval/src/eval_brain.rs
git commit -m "refactor(eval): 评估脑重构为 tool_loop 循环架构，统一有/无文件变更路径"
```

---

### Task 4: 清理和回归测试

**Files:**
- Modify: `rust/crates/brain-eval/src/prompts.rs` (删除未使用的 `format_file_changes` 如果无其他调用者)
- Modify: `rust/crates/brain-eval/src/eval_brain.rs` (清理 dead code)

**Step 1: 检查 `format_file_changes` 是否还有调用者**

Run: `cd rust && grep -r "format_file_changes" --include="*.rs"`
如果只有 prompts.rs 中的定义和测试，可以安全删除。

**Step 2: 删除未使用代码**

- 删除 `format_file_changes()` 函数及其测试
- 删除 `extractor` 的未使用 import（如果 eval_brain.rs 不再直接调用 `extractor::extract_file_changes`，但保留 `use crate::extractor` 因为还在 evaluate 中用于 progress 事件计数）

**Step 3: 运行 clippy**

Run: `cd rust && cargo clippy -p brain-eval --all-targets -- -D warnings 2>&1 | tail -20`
Expected: 无 warning

**Step 4: 运行完整 workspace 测试**

Run: `cd rust && cargo test --workspace 2>&1 | tail -30`
Expected: all tests passed

**Step 5: 提交**

```bash
git add rust/crates/brain-eval/
git commit -m "chore(eval): 清理评估脑重构后的未使用代码"
```
