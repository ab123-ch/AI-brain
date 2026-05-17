# 评估脑 v2 — 独立验证能力 实现计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 给评估脑增加只读工具访问能力（read_file, grep_search, glob_search），使其能独立验证主脑的工作成果。

**Architecture:** 评估脑从"纯文本审查"升级为"带工具验证的审查"。流程变为：主脑完成 → 提取文件变更 → Round1 LLM（带只读工具定义）→ LLM 可选调用工具验证 → 执行工具 → Round2 LLM（带工具证据）→ 出评估结论。每轮独立构建 ChatRequest（同一 system prompt 前缀保证 cache hit），最多 2 次 LLM 调用，不需要上下文压缩。

**Tech Stack:** Rust, brain-eval crate, brain-llm (ChatMessage/ContentBlock/ToolDefinition), brain-core (ToolExecutor/TurnRecord)

---

## 设计要点

1. **有界验证**：最多 1 轮工具调用（2 次 LLM 调用），不会无限膨胀
2. **只读安全**：评估脑只能用 read_file / grep_search / glob_search，双保险（只给只读工具定义 + 执行时校验工具名）
3. **优雅降级**：无 tool_executor 或无文件变更时，回退到现有纯文本评估
4. **Cache 友好**：两轮 LLM 使用同一 system prompt 前缀，Round2 天然命中 Round1 的缓存
5. **eval_requirements 兼容**：新增的验证维度通过 eval_requirements 动态积累，不改 Rust 代码

---

## 修改文件清单

| 操作 | 文件 |
|------|------|
| 创建 | `rust/crates/brain-eval/src/extractor.rs` |
| 修改 | `rust/crates/brain-eval/src/lib.rs` |
| 修改 | `rust/crates/brain-eval/src/prompts.rs` |
| 修改 | `rust/crates/brain-eval/src/eval_brain.rs` |
| 修改 | `rust/crates/brain-eval/src/error.rs` |
| 修改 | `rust/crates/ai-brain-cli/src/orchestrator.rs` |

---

### Task 1: 文件变更提取器 (extractor.rs)

从主脑的 `TurnRecord` 中提取文件变更记录，供评估脑了解主脑改了什么。

**Files:**
- 创建: `rust/crates/brain-eval/src/extractor.rs`
- 修改: `rust/crates/brain-eval/src/lib.rs`

**Step 1: 写 extractor.rs 的类型和函数**

```rust
// rust/crates/brain-eval/src/extractor.rs

use brain_core::types::{ToolCallRecord, TurnRecord, TurnRole};

/// 单个文件变更记录
#[derive(Debug, Clone)]
pub struct FileChange {
    /// 文件路径
    pub file_path: String,
    /// 变更类型
    pub change_type: FileChangeType,
    /// edit_file 的 old_string（仅 ChangeType::Edit 有值）
    pub old_content: Option<String>,
    /// edit_file 的 new_string（仅 ChangeType::Edit 有值）
    pub new_content: Option<String>,
}

/// 变更类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeType {
    /// 编辑（edit_file）
    Edit,
    /// 新建/覆盖（write_file）
    Write,
}

/// 从 TurnRecord 列表中提取所有文件变更
pub fn extract_file_changes(turns: &[TurnRecord]) -> Vec<FileChange> {
    turns
        .iter()
        .filter(|t| t.role == TurnRole::ToolCall && !t.is_error())
        .filter_map(|t| {
            let tc = t.tool_call.as_ref()?;
            match tc.tool_name.as_str() {
                "edit_file" => extract_edit(&tc),
                "write_file" => extract_write(&tc),
                _ => None,
            }
        })
        .collect()
}

fn extract_edit(tc: &ToolCallRecord) -> Option<FileChange> {
    let path = tc.input.get("file_path")?.as_str()?.to_string();
    let old = tc.input.get("old_string").and_then(|v| v.as_str()).map(String::from);
    let new = tc.input.get("new_string").and_then(|v| v.as_str()).map(String::from);
    Some(FileChange {
        file_path: path,
        change_type: FileChangeType::Edit,
        old_content: old,
        new_content: new,
    })
}

fn extract_write(tc: &ToolCallRecord) -> Option<FileChange> {
    let path = tc.input.get("file_path")?.as_str()?.to_string();
    Some(FileChange {
        file_path: path,
        change_type: FileChangeType::Write,
        old_content: None,
        new_content: None, // write_file 内容太大，不传入 prompt
    })
}
```

注意：`TurnRecord` 没有 `is_error()` 方法，`is_error` 在 `ToolCallRecord` 上。需要在 `extract_file_changes` 中通过 `tc.is_error` 过滤。

**Step 2: 给 TurnRecord 添加 is_error 判断 helper**

实际上 `TurnRecord` 的 `tool_call: Option<ToolCallRecord>` 中，`ToolCallRecord` 有 `is_error: bool` 字段。在 extractor 中直接通过 `t.tool_call.as_ref().map_or(true, |tc| !tc.is_error)` 过滤即可，不需要改 brain-core。

修正后的过滤逻辑：

```rust
pub fn extract_file_changes(turns: &[TurnRecord]) -> Vec<FileChange> {
    turns
        .iter()
        .filter(|t| {
            t.role == TurnRole::ToolCall
                && t.tool_call.as_ref().map_or(false, |tc| !tc.is_error)
        })
        .filter_map(|t| {
            let tc = t.tool_call.as_ref()?;
            match tc.tool_name.as_str() {
                "edit_file" => extract_edit(tc),
                "write_file" => extract_write(tc),
                _ => None,
            }
        })
        .collect()
}
```

**Step 3: 写测试**

在 `extractor.rs` 底部添加 `#[cfg(test)] mod tests`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::ToolCallRecord;
    use serde_json::json;

    fn make_edit_turn(path: &str, old: &str, new: &str, is_error: bool) -> TurnRecord {
        TurnRecord {
            role: TurnRole::ToolCall,
            content: String::new(),
            tool_call: Some(ToolCallRecord {
                tool_name: "edit_file".into(),
                input: json!({
                    "file_path": path,
                    "old_string": old,
                    "new_string": new,
                }),
                output: "OK".into(),
                duration_ms: 10,
                is_error,
            }),
            timestamp: String::new(),
        }
    }

    fn make_write_turn(path: &str, is_error: bool) -> TurnRecord {
        TurnRecord {
            role: TurnRole::ToolCall,
            content: String::new(),
            tool_call: Some(ToolCallRecord {
                tool_name: "write_file".into(),
                input: json!({
                    "file_path": path,
                    "content": "fn main() {}",
                }),
                output: "OK".into(),
                duration_ms: 10,
                is_error,
            }),
            timestamp: String::new(),
        }
    }

    fn make_grep_turn() -> TurnRecord {
        TurnRecord {
            role: TurnRole::ToolCall,
            content: String::new(),
            tool_call: Some(ToolCallRecord {
                tool_name: "grep_search".into(),
                input: json!({"pattern": "TODO"}),
                output: "3 matches".into(),
                duration_ms: 50,
                is_error: false,
            }),
            timestamp: String::new(),
        }
    }

    #[test]
    fn extract_edit_changes() {
        let turns = vec![
            make_edit_turn("src/a.rs", "fn old()", "fn new()", false),
            make_grep_turn(),
            make_edit_turn("src/b.rs", "old_val", "new_val", false),
        ];
        let changes = extract_file_changes(&turns);
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].file_path, "src/a.rs");
        assert_eq!(changes[0].change_type, FileChangeType::Edit);
        assert_eq!(changes[0].old_content.as_deref(), Some("fn old()"));
        assert_eq!(changes[0].new_content.as_deref(), Some("fn new()"));
        assert_eq!(changes[1].file_path, "src/b.rs");
    }

    #[test]
    fn extract_write_changes() {
        let turns = vec![make_write_turn("src/new.rs", false)];
        let changes = extract_file_changes(&turns);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change_type, FileChangeType::Write);
        assert_eq!(changes[0].old_content, None);
    }

    #[test]
    fn skips_failed_tool_calls() {
        let turns = vec![
            make_edit_turn("src/a.rs", "old", "new", true),  // failed
            make_edit_turn("src/b.rs", "old", "new", false), // succeeded
        ];
        let changes = extract_file_changes(&turns);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].file_path, "src/b.rs");
    }

    #[test]
    fn skips_non_file_tools() {
        let turns = vec![make_grep_turn()];
        let changes = extract_file_changes(&turns);
        assert!(changes.is_empty());
    }

    #[test]
    fn empty_turns() {
        let changes = extract_file_changes(&[]);
        assert!(changes.is_empty());
    }
}
```

**Step 4: 在 lib.rs 中注册模块**

```rust
// 在 lib.rs 中添加
pub(crate) mod extractor;
```

**Step 5: 运行测试确认通过**

```bash
cd rust && cargo test -p brain-eval -- extractor
```

**Step 6: Commit**

```bash
git add rust/crates/brain-eval/src/extractor.rs rust/crates/brain-eval/src/lib.rs
git commit -m "feat(eval): add file change extractor from TurnRecord"
```

---

### Task 2: 只读工具定义与安全检查

评估脑使用的工具定义 + 执行时安全校验。

**Files:**
- 修改: `rust/crates/brain-eval/src/eval_brain.rs`
- 修改: `rust/crates/brain-eval/Cargo.toml`

**Step 1: 在 eval_brain.rs 中添加只读工具定义函数**

在 `eval_brain.rs` 顶部（impl 块之前）添加：

```rust
use brain_llm::{ToolDefinition, ToolChoice};

/// 评估脑允许使用的只读工具白名单
const READ_ONLY_TOOLS: &[&str] = &["read_file", "grep_search", "glob_search"];

/// 检查工具名是否在只读白名单中
fn is_read_only_tool(name: &str) -> bool {
    READ_ONLY_TOOLS.contains(&name)
}

/// 构建评估脑专用的只读工具定义
///
/// 精简版：只暴露 LLM 需要的核心参数，避免 eval 脑误用高级参数
pub fn build_read_only_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "read_file".into(),
            description: "读取文件内容（只读）。可以指定行范围。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "要读取的文件的绝对路径"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "从第几行开始读取（可选，默认从第一行）"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "最多读取多少行（可选，默认全部）"
                    }
                },
                "required": ["file_path"]
            }),
        },
        ToolDefinition {
            name: "grep_search".into(),
            description: "在文件内容中搜索匹配正则表达式的行（只读）。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "正则表达式搜索模式"
                    },
                    "path": {
                        "type": "string",
                        "description": "搜索目录（可选，默认当前目录）"
                    },
                    "output_mode": {
                        "type": "string",
                        "description": "输出模式：content（显示匹配行）或 files_with_matches（只显示文件名）"
                    },
                    "head_limit": {
                        "type": "integer",
                        "description": "最多返回多少条结果（建议设为20以内）"
                    }
                },
                "required": ["pattern"]
            }),
        },
        ToolDefinition {
            name: "glob_search".into(),
            description: "按 glob 模式搜索文件路径（只读）。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "glob 模式，如 **/*.rs 或 src/**/*.ts"
                    },
                    "path": {
                        "type": "string",
                        "description": "搜索目录（可选，默认当前目录）"
                    }
                },
                "required": ["pattern"]
            }),
        },
    ]
}
```

**Step 2: 写测试**

```rust
#[test]
fn read_only_tool_whitelist() {
    assert!(is_read_only_tool("read_file"));
    assert!(is_read_only_tool("grep_search"));
    assert!(is_read_only_tool("glob_search"));
    assert!(!is_read_only_tool("edit_file"));
    assert!(!is_read_only_tool("write_file"));
    assert!(!is_read_only_tool("bash"));
}

#[test]
fn build_read_only_tools_has_three() {
    let tools = build_read_only_tool_definitions();
    assert_eq!(tools.len(), 3);
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"read_file"));
    assert!(names.contains(&"grep_search"));
    assert!(names.contains(&"glob_search"));
}
```

**Step 3: 运行测试**

```bash
cd rust && cargo test -p brain-eval -- read_only
```

**Step 4: Commit**

```bash
git add rust/crates/brain-eval/src/eval_brain.rs
git commit -m "feat(eval): add read-only tool definitions and safety check"
```

---

### Task 3: 验证感知的评估 Prompt

修改 prompts.rs，新增文件变更段落和工具验证指引。

**Files:**
- 修改: `rust/crates/brain-eval/src/prompts.rs`

**Step 1: 添加 FileChange 导入和格式化函数**

在 `prompts.rs` 顶部添加：

```rust
use crate::extractor::{FileChange, FileChangeType};
```

**Step 2: 修改 build_evaluation_system_prompt**

在第一段"角色定义"后、第二段"评估维度"前，插入工具验证说明：

```rust
    // ── 第一段和第二段之间插入 ──
    if with_tools {
        prompt.push_str(r#"# 验证工具

你可以使用以下只读工具来验证主脑的工作成果：
- **read_file** — 读取文件内容，检查代码是否正确、位置是否正确
- **grep_search** — 搜索代码内容，检查调用链、引用关系
- **glob_search** — 搜索文件路径，检查项目结构

## 验证策略
- 如果用户要求修改代码，你应该验证：代码确实被修改、修改位置正确、修改内容符合预期
- 如果修改涉及函数定义变更，你应该 grep 搜索该函数名，确认调用方是否受影响
- 如果主脑输出包含"已修改"的声明，你应该 read_file 确认修改确实存在
- 如果不需要验证（闲聊、问答、无代码修改），直接给出评估结果

"#);
    }
```

函数签名改为：

```rust
pub fn build_evaluation_system_prompt(eval_requirements: &[EvalRequirement], with_tools: bool) -> String {
```

**Step 3: 添加文件变更格式化函数**

```rust
/// 将文件变更列表格式化为 prompt 文本
pub fn format_file_changes(changes: &[FileChange]) -> String {
    if changes.is_empty() {
        return String::new();
    }

    let mut s = String::from("## 文件修改记录（主脑本轮操作）\n\n");
    for (i, c) in changes.iter().enumerate() {
        match c.change_type {
            FileChangeType::Edit => {
                let _ = writeln!(s, "{}. 编辑 `{}`", i + 1, c.file_path);
                if let Some(ref old) = c.old_content {
                    // 截断过长的内容
                    let truncated: String = old.chars().take(500).collect();
                    let _ = writeln!(s, "   替换前: {}", truncated);
                }
                if let Some(ref new) = c.new_content {
                    let truncated: String = new.chars().take(500).collect();
                    let _ = writeln!(s, "   替换后: {}", truncated);
                }
            }
            FileChangeType::Write => {
                let _ = writeln!(s, "{}. 写入 `{}`（新建或覆盖）", i + 1, c.file_path);
            }
        }
    }
    s.push('\n');
    s
}
```

**Step 4: 修改 build_evaluation_user_prompt**

在"主脑输出"段落之后、"踩坑库"段落之前，插入文件变更：

```rust
pub fn build_evaluation_user_prompt(
    user_input: &str,
    ai_output: &str,
    pitfalls: &[PitfallRecord],
    user_profile: &UserProfile,
    rules: &[EvolutionRule],
    file_changes: &[FileChange],  // 新增参数
) -> String {
```

在 `ai_output` 段落之后添加：

```rust
    // 文件修改记录
    let changes_text = format_file_changes(file_changes);
    if !changes_text.is_empty() {
        prompt.push_str(&changes_text);
    }
```

**Step 5: 更新现有测试适配新参数**

所有调用 `build_evaluation_system_prompt` 的地方加 `false` 参数（不带工具）。
所有调用 `build_evaluation_user_prompt` 的地方加 `&[]` 参数（空文件变更）。

这是纯机械修改，不影响行为。例如：

```rust
// 之前
let prompt = build_evaluation_system_prompt(&[]);
// 之后
let prompt = build_evaluation_system_prompt(&[], false);

// 之前
let prompt = build_evaluation_user_prompt("input", "output", &[], &UserProfile::default(), &[]);
// 之后
let prompt = build_evaluation_user_prompt("input", "output", &[], &UserProfile::default(), &[], &[]);
```

**Step 6: 添加新测试**

```rust
#[test]
fn system_prompt_with_tools_has_verification_section() {
    let prompt = build_evaluation_system_prompt(&[], true);
    assert!(prompt.contains("验证工具"));
    assert!(prompt.contains("read_file"));
    assert!(prompt.contains("grep_search"));
}

#[test]
fn system_prompt_without_tools_no_verification_section() {
    let prompt = build_evaluation_system_prompt(&[], false);
    assert!(!prompt.contains("验证工具"));
}

#[test]
fn user_prompt_with_file_changes() {
    let changes = vec![
        FileChange {
            file_path: "src/main.rs".into(),
            change_type: FileChangeType::Edit,
            old_content: Some("fn old()".into()),
            new_content: Some("fn new() {}".into()),
        },
    ];
    let prompt = build_evaluation_user_prompt(
        "改代码", "已修改", &[], &UserProfile::default(), &[], &changes,
    );
    assert!(prompt.contains("文件修改记录"));
    assert!(prompt.contains("src/main.rs"));
    assert!(prompt.contains("fn new()"));
}

#[test]
fn user_prompt_without_file_changes_no_section() {
    let prompt = build_evaluation_user_prompt(
        "闲聊", "你好", &[], &UserProfile::default(), &[], &[],
    );
    assert!(!prompt.contains("文件修改记录"));
}
```

**Step 7: 运行全部 prompts 测试**

```bash
cd rust && cargo test -p brain-eval -- prompts
```

**Step 8: Commit**

```bash
git add rust/crates/brain-eval/src/prompts.rs
git commit -m "feat(eval): add verification-aware evaluation prompts with file changes"
```

---

### Task 4: EvalBrain evaluate_with_verification 实现

核心：给 EvalBrain 加 tool_executor，实现带工具验证的两轮评估。

**Files:**
- 修改: `rust/crates/brain-eval/src/eval_brain.rs`
- 修改: `rust/crates/brain-eval/src/error.rs`

**Step 1: 在 error.rs 中添加工具执行错误变体**

```rust
/// 工具执行失败
#[error("tool execution failed: {0}")]
ToolError(String),
```

**Step 2: 修改 EvalBrain 结构体，添加 tool_executor 字段**

```rust
use std::sync::Arc;
use brain_core::tool_executor::ToolExecutor;
use brain_core::types::{ToolCall, TurnRecord};
use brain_llm::{ChatMessage, ChatRequest, ContentBlock, LlmProvider, ToolChoice, ToolDefinition};
use crate::extractor;
use crate::prompts;

pub struct EvalBrain {
    llm: Arc<dyn LlmProvider>,
    tool_executor: Option<Arc<dyn ToolExecutor>>,
    progress_tx: Option<tokio::sync::mpsc::Sender<brain_core::types::ProgressEvent>>,
}
```

**Step 3: 修改构造函数，添加 with_verification**

```rust
impl EvalBrain {
    /// 创建评估脑实例（纯文本评估，无工具验证）
    pub fn new(llm: Arc<dyn LlmProvider>) -> Self {
        Self {
            llm,
            tool_executor: None,
            progress_tx: None,
        }
    }

    /// 创建带工具验证能力的评估脑实例
    pub fn with_verification(
        llm: Arc<dyn LlmProvider>,
        tool_executor: Arc<dyn ToolExecutor>,
    ) -> Self {
        Self {
            llm,
            tool_executor: Some(tool_executor),
            progress_tx: None,
        }
    }
```

**Step 4: 实现 evaluate_with_verification**

在 `evaluate()` 方法之后添加：

```rust
    /// 带工具验证的评估
    ///
    /// 流程：
    /// 1. 提取文件变更 → 无变更时降级到纯文本评估
    /// 2. Round1: LLM 分析，可选调用只读工具
    /// 3. 执行工具（只允许 read_only 白名单）
    /// 4. Round2: LLM 基于工具证据出最终评估
    pub async fn evaluate_with_verification(
        &self,
        user_input: &str,
        ai_output: &str,
        turns: &[TurnRecord],
        pitfalls: &[PitfallRecord],
        user_profile: &UserProfile,
        rules: &[EvolutionRule],
        eval_requirements: &[EvalRequirement],
    ) -> Result<EvalResult> {
        // 降级条件：无 tool_executor 或无文件变更
        let file_changes = extractor::extract_file_changes(turns);
        if self.tool_executor.is_none() || file_changes.is_empty() {
            return self.evaluate(
                user_input, ai_output, pitfalls, user_profile, rules, eval_requirements,
            ).await;
        }

        if user_input.trim().is_empty() || ai_output.trim().is_empty() {
            return Err(EvalError::InvalidInput(
                "user_input and ai_output must not be empty".into(),
            ));
        }

        // 发送评估开始事件
        if let Some(tx) = &self.progress_tx {
            let _ = tx.try_send(ProgressEvent::EvaluationStart);
        }

        let has_tools = self.tool_executor.is_some() && !file_changes.is_empty();
        let system_prompt = prompts::build_evaluation_system_prompt(eval_requirements, has_tools);
        let user_prompt = prompts::build_evaluation_user_prompt(
            user_input, ai_output, pitfalls, user_profile, rules, &file_changes,
        );

        // ── Round 1: 带工具定义，LLM 可选调用工具 ──
        let read_only_tools = build_read_only_tool_definitions();
        let messages = vec![
            ChatMessage::system(&system_prompt),
            ChatMessage::user(&user_prompt),
        ];
        let request = ChatRequest {
            model: None,
            messages,
            max_tokens: Some(2048),
            temperature: Some(0.1),
            tools: Some(read_only_tools),
            tool_choice: Some(ToolChoice::Auto),
        };

        let response = self
            .llm
            .complete(request)
            .await
            .map_err(|e| EvalError::LlmError(e.to_string()))?;

        // LLM 没有调用工具 → 直接解析为评估结果
        if !response.has_tool_calls() {
            let feedback = response.text();
            let passed = !feedback.contains("存在问题");
            self.emit_result(passed, &feedback);
            return Ok(EvalResult { passed, feedback: feedback.trim().to_string() });
        }

        // ── 执行验证工具 ──
        let tool_executor = self.tool_executor.as_ref().unwrap();
        let mut messages = vec![
            ChatMessage::system(&system_prompt),
            ChatMessage::user(&user_prompt),
            ChatMessage::assistant_blocks(response.content.clone()),
        ];

        for tool_block in response.tool_calls() {
            if let ContentBlock::ToolUse { id, name, input } = tool_block {
                // 安全校验：只允许只读工具
                if !is_read_only_tool(name) {
                    tracing::warn!("评估脑工具安全拒绝: {name}");
                    messages.push(ChatMessage::tool_result(id, format!("工具 {name} 不可用：评估脑只允许只读工具"), true));
                    continue;
                }

                let tool_call = ToolCall {
                    tool_name: name.clone(),
                    input: input.clone(),
                    validated: false,
                    validation_id: None,
                };

                tracing::info!("评估脑验证工具: {name}");
                let result = tool_executor.execute(&tool_call).await
                    .map_err(|e| EvalError::ToolError(e.to_string()))?;

                // 截断工具输出（评估脑不需要超大输出）
                let output = truncate_verification_output(&result.output, 5000);

                messages.push(ChatMessage::tool_result(id, output, result.is_error));
            }
        }

        // ── Round 2: 带工具证据，无工具定义，出最终评估 ──
        let request = ChatRequest {
            model: None,
            messages,
            max_tokens: Some(2048),
            temperature: Some(0.1),
            tools: None,
            tool_choice: None,
        };

        let response = self
            .llm
            .complete(request)
            .await
            .map_err(|e| EvalError::LlmError(e.to_string()))?;

        let feedback = response.text();
        if feedback.trim().is_empty() {
            self.emit_result(true, "评估结果-正常");
            return Ok(EvalResult::passed());
        }

        let passed = !feedback.contains("存在问题");
        self.emit_result(passed, &feedback);
        Ok(EvalResult { passed, feedback: feedback.trim().to_string() })
    }
```

**Step 5: 添加输出截断辅助函数**

```rust
/// 截断验证工具输出，防止大结果撑爆评估脑上下文
fn truncate_verification_output(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    format!("{truncated}\n\n[输出已截断，原始 {} 字符，保留前 {} 字符]",
        s.chars().count(), max_chars)
}
```

**Step 6: 更新 llm_evaluate 调用适配新 prompt 签名**

现有 `llm_evaluate` 方法调用 `build_evaluation_system_prompt(eval_requirements)` 和 `build_evaluation_user_prompt(user_input, ai_output, pitfalls, user_profile, rules)`。需要更新为：

```rust
let system_prompt = prompts::build_evaluation_system_prompt(eval_requirements, false);
let user_prompt = prompts::build_evaluation_user_prompt(
    user_input, ai_output, pitfalls, user_profile, rules, &[],
);
```

**Step 7: 写集成测试**

```rust
#[tokio::test]
async fn evaluate_with_verification_no_changes_falls_back() {
    // 无文件变更时降级到纯文本评估
    let llm = Arc::new(MockLlmProvider::new("评估结果-正常"));
    let executor = Arc::new(brain_core::tool_executor::StubToolExecutor::new());
    let brain = EvalBrain::with_verification(llm, executor);

    let result = brain
        .evaluate_with_verification(
            "写代码", "fn add() {}", &[],  // turns 为空
            &[], &UserProfile::default(), &[], &[],
        )
        .await
        .unwrap();
    assert!(result.passed);
}

#[tokio::test]
async fn evaluate_with_verification_calls_tools() {
    // LLM 第一轮调用 read_file → 第二轮出评估
    use std::sync::atomic::{AtomicUsize, Ordering};
    let call_count = Arc::new(AtomicUsize::new(0));
    let count_clone = call_count.clone();

    struct TwoRoundLlm {
        call_count: Arc<AtomicUsize>,
    }
    impl LlmProvider for TwoRoundLlm {
        fn model(&self) -> &'static str { "mock" }
        fn complete(
            &self, _request: ChatRequest,
        ) -> Pin<Box<dyn Future<Output = brain_llm::Result<ChatResponse>> + Send + '_>> {
            let count = self.call_count.clone();
            Box::pin(async move {
                let n = count.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    // Round 1: 返回工具调用
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
                } else {
                    // Round 2: 返回评估结果
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

    let llm = Arc::new(TwoRoundLlm { call_count: count_clone });
    let executor = Arc::new(brain_core::tool_executor::StubToolExecutor::new());
    let brain = EvalBrain::with_verification(llm, executor);

    // 构造包含 edit_file 的 turns
    let turns = vec![TurnRecord {
        role: TurnRole::ToolCall,
        content: String::new(),
        tool_call: Some(ToolCallRecord {
            tool_name: "edit_file".into(),
            input: serde_json::json!({"file_path": "/tmp/test.rs", "old_string": "old", "new_string": "new"}),
            output: "OK".into(),
            duration_ms: 10,
            is_error: false,
        }),
        timestamp: String::new(),
    }];

    let result = brain
        .evaluate_with_verification(
            "改代码", "已修改", &turns,
            &[], &UserProfile::default(), &[], &[],
        )
        .await
        .unwrap();

    assert!(result.passed);
    assert_eq!(call_count.load(Ordering::SeqCst), 2); // 确认两轮 LLM
}
```

**Step 8: 运行全部 eval_brain 测试**

```bash
cd rust && cargo test -p brain-eval
```

**Step 9: Commit**

```bash
git add rust/crates/brain-eval/src/eval_brain.rs rust/crates/brain-eval/src/error.rs
git commit -m "feat(eval): implement evaluate_with_verification with read-only tools"
```

---

### Task 5: Orchestrator 接入

将评估脑 v2 接入 orchestrator 的 query_streaming 流程。

**Files:**
- 修改: `rust/crates/ai-brain-cli/src/orchestrator.rs`

**Step 1: 修改 EvalBrain 构造，传入 tool_executor**

找到 orchestrator.rs 中 `EvalBrain::new` 的位置（约第 215-223 行），改为：

```rust
// 4.1 创建 v2 评估脑（LLM 深度评估 + 只读工具验证）
let tool_executor_for_eval: Arc<dyn brain_core::tool_executor::ToolExecutor> = Arc::new(
    crate::real_tool_executor::RealToolExecutor::with_memory(memory_brain.clone()),
);
let eval_brain = if let Ok(config) = LlmConfig::load_default() {
    if let Ok(client) = config.create_brain_client("eval") {
        Some(EvalBrain::with_verification(
            Arc::from(client),
            tool_executor_for_eval,
        ))
    } else {
        None
    }
} else {
    None
};
```

**Step 2: 修改评估调用，传入 turns**

找到 `query_streaming` 中调用 `eb.evaluate` 的位置（约第 708 行），改为：

```rust
match eb
    .evaluate_with_verification(
        &input_owned,
        &answer,
        &result.as_ref().unwrap().turns,  // 新增：传入工具调用轨迹
        &pitfalls,
        &profile,
        &rules,
        &eval_requirements,
    )
    .await
```

**Step 3: 运行编译确认**

```bash
cd rust && cargo check -p ai-brain-cli
```

**Step 4: 运行全部 workspace 测试**

```bash
cd rust && cargo test --workspace --exclude brain-integration-tests
```

**Step 5: Commit**

```bash
git add rust/crates/ai-brain-cli/src/orchestrator.rs
git commit -m "feat: wire eval brain v2 with verification into orchestrator"
```

---

### Task 6: 端到端验证

手动或集成测试验证整体流程。

**Step 1: cargo clippy 检查**

```bash
cd rust && cargo clippy --workspace --all-targets -- -D warnings
```

修复所有 clippy 警告。

**Step 2: cargo fmt 格式化**

```bash
cd rust && cargo fmt
```

**Step 3: 全量测试**

```bash
cd rust && cargo test --workspace --exclude brain-integration-tests
```

**Step 4: 运行时验证**

启动 TUI，给主脑一个修改代码的任务（如"修改 token 计算逻辑"），观察：
1. 日志中出现"评估脑验证工具: read_file"或"评估脑验证工具: grep_search"
2. 评估脑能正确识别代码修改并通过评估
3. 或正确识别代码位置错误/未调用等问题

**Step 5: 最终 Commit**

```bash
git add -A
git commit -m "feat(eval): eval brain v2 with independent verification - complete"
```

---

## 向后兼容说明

- `EvalBrain::new()` 保持不变（纯文本评估）
- `EvalBrain::with_verification()` 是新增构造函数
- `evaluate()` 方法保持不变（内部调用签名不变）
- `evaluate_with_verification()` 是新增方法
- `prompts.rs` 的函数签名有变化（加了参数），但现有调用方在 Task 3/4 中已全部更新
