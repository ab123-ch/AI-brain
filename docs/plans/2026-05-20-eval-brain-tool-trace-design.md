# 评估脑操作轨迹可见性设计

## 日期
2026-05-20

## 问题

评估脑在评估主脑输出时"没事找事"，核心原因是看不到主脑的工具调用和结果。

### 根因分析

1. `tool_loop` 每轮生成 `turns: Vec<TurnRecord>`，包含所有工具调用（bash/grep/WebSearch/edit_file 等）
2. orchestrator 把 `turns` 传给 `eval_brain.evaluate()`
3. **但 `extractor::extract_file_changes()` 只提取 `edit_file`/`write_file`，其余全部丢弃**
4. `build_evaluation_user_prompt()` 只注入 `file_changes`
5. 评估脑看不到主脑用了 bash/WebSearch 等工具，自然会误判"主脑没遵守用户指令"

### 实际案例

日志 `sess-1779251492.jsonl`：
- 用户说"用bash吧"
- 主脑确实调用了 bash（`curl -s wttr.in/Ningbo`）并成功
- 评估脑看不到 bash 调用 → 判定 FAIL："主脑未遵守用户明确指令'用bash吧'"
- 主脑被迫重新执行一遍相同操作

### 作用域隔离

`turns` 参数天然隔离了两个问题：
- **不会读到别的会话**：turns 来自当前 `process_input()` 调用
- **不会读到前几轮**：`tool_loop` 每次 `process_input` 都从空 Vec 新建（`tool_loop.rs:127`）

## 设计方案

### 架构变更：评估脑从"固定两轮"重构为"tool_loop 循环"

评估脑应该和主脑一样使用 tool_loop 循环，唯一区别是工具白名单为只读。

#### 当前架构（固定两轮）

```
Round 1: LLM（带只读工具定义）→ 可选调工具
执行工具
Round 2: LLM（无工具定义）→ 强制出最终评估
```

#### 新架构（tool_loop 循环）

```
构建上下文（system prompt + user prompt + 工具调用轨迹摘要）
    ↓
eval_tool_loop（循环直到 LLM 不再调工具，最多 10 轮）
    - LLM 自主决定调多少轮工具
    - 工具白名单：read_file / grep_search / glob_search / Skill / bash
    - bash 二级白名单不变
    - Skill 从 SkillRegistry 读取内容
    ↓
解析最终文本：包含"存在问题" → FAIL，否则 → PASS
```

### tool_loop 复用策略：内部简化版

在 `eval_brain.rs` 内部新增 `eval_tool_loop()` 函数，不复用 `brain-main` 的 `tool_loop`。

原因：
- 评估脑不需要 hook_runner、guard_check、progress 事件发送
- 避免 brain-eval ↔ brain-main 循环依赖
- 代码量约 80 行，不值得提取公共函数

### 工具调用轨迹摘要格式

在 user prompt 中新增 `## 主脑操作轨迹` 段，格式化所有工具调用：

```
## 主脑操作轨迹
1. [bash] curl -s wttr.in/Ningbo → 成功(1249ms)
   输入: {"command":"curl -s wttr.in/Ningbo"}
   输出摘要: Weather report: Ningbo...Partly Cloudy +24°C...
2. [bash] curl -s "wttr.in/Ningbo?format=..." → 失败(0ms)
   输入: {"command":"curl -s \"wttr.in/Ningbo?format=%l:+%c+%t\""}
   错误: 安全拒绝: Bash command contains potentially destructive pattern: "format"
3. [edit_file] src/main.rs → 成功(10ms)
   替换前: fn old()
   替换后: fn new() {}
```

规则：
- 所有工具调用都展示（不限于 edit_file/write_file）
- 输入 JSON 完整展示（通常很短）
- 成功输出截断到 300 字符
- 失败输出完整展示（错误信息通常很短）
- edit_file/write_file 保留 old/new 内容（前 500 字符）

### 统一降级路径

不再区分"有/无文件变更"两条路径。无论主脑是否改了文件，评估脑都走同一个 `eval_tool_loop`。

唯一降级：没有 `tool_executor`（纯内存模式）时，走单次 LLM 调用（不暴露工具）。

## 改动文件清单

| 文件 | 改动类型 | 内容 |
|------|---------|------|
| `brain-eval/src/eval_brain.rs` | 重写 | 新增 `eval_tool_loop()`；`evaluate()` 改为调用它；删除 `llm_evaluate_fallback()` 和固定两轮逻辑 |
| `brain-eval/src/prompts.rs` | 修改 | 新增 `format_tool_trace(turns)` 格式化所有工具调用；`build_evaluation_user_prompt()` 参数从 `file_changes: &[FileChange]` 改为 `turns: &[TurnRecord]` |
| `brain-eval/src/extractor.rs` | 保留 | `extract_file_changes()` 保留，仅供 Skill 内部需要文件列表时使用 |

**不改动**：`brain-core`、`brain-main`、`orchestrator.rs`、`session_logger.rs`

## token 消耗控制

1. 工具输出截断：5000 字符上限（保持现有）
2. 轨迹摘要输出截断：每个工具调用 300 字符
3. eval_tool_loop 最大轮次：10
4. 每次 LLM 调用 max_tokens：2048（保持现有）

## 测试策略

- 保留现有所有测试（MockLLM 适配新架构）
- 新增：eval_tool_loop 多轮工具调用测试
- 新增：format_tool_trace 格式化测试（覆盖各种工具类型）
- 新增：降级路径测试（无 tool_executor）
