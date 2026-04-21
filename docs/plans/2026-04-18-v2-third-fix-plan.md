# AI Brain v2 — 第三轮自检修复清单

## 背景

第二轮修复完成了 S1-S5（29/38 项）。第三轮全面排查发现 5 高 + 9 中 + 12 低 = 26 个问题。
本轮聚焦核心功能断裂和设计不符，延后低优先级项。

## 修复任务分组（按优先级）

### T1 — 核心功能断裂（必须修）

| # | 问题 | 修复方案 | 涉及文件 |
|---|------|---------|---------|
| T1-1 | REPL 流式路径缺少记忆召回注入（query_streaming 没调 recall_for_context） | 在 query_streaming 中与 query 路径对称：先调 recall_for_context 注入记忆，再调主脑 | `ai-brain-cli/src/orchestrator.rs` |
| T1-2 | 两条路径分析触发计数器不同步（streaming 用 query_count，query 用 should_trigger_analysis） | streaming 路径也改用 should_trigger_analysis，删除独立的 is_multiple_of 逻辑 | `ai-brain-cli/src/orchestrator.rs` |
| T1-3 | RecallEngine 不更新使用计数（recall 后不调 increment_use/increment_recall） | recall 方法在返回结果后调用 increment_use/increment_recall 更新使用热度 | `brain-memory/src/recall.rs` |
| T1-4 | read_all_unconsolidated 读全量而非增量（方法名误导 + RawEntry 缺 consolidated 标记） | 给 RawEntry 添加 consolidated 字段，巩固后标记，read_all_unconsolidated 只读未标记条目 | `brain-memory/src/raw_layer.rs`, `brain-memory/src/consolidation.rs` |

### T2 — 设计不符修复

| # | 问题 | 修复方案 | 涉及文件 |
|---|------|---------|---------|
| T2-1 | EvalResult::failed() 名字与行为矛盾 | 重命名为 from_issues()，语义更准确 | `brain-eval/src/eval_brain.rs` |
| T2-2 | max_keywords 配置传了但 extract_keywords 硬编码 5 | extract_keywords 使用 self.config.max_keywords | `brain-memory/src/memory_brain.rs` |
| T2-3 | process_input_streaming 用 run_tool_loop 而非 _with_config | 改为调用 run_tool_loop_with_config | `brain-main/src/main_brain.rs` |
| T2-4 | llm_max_tokens/llm_temperature 硬编码不读 BrainConfig | 从 BrainConfig 新增 llm_defaults section 读取，或从 LlmConfig 获取 | `brain-main/src/main_brain.rs`, `brain-core/src/config.rs` |
| T2-5 | ThresholdConfig 3 个字段未使用（consolidation_importance/memory_recall_min/context_warning） | 接入或删除 | `brain-core/src/config.rs` |

### T3 — 中等优先级（功能补全）

| # | 问题 | 修复方案 | 涉及文件 |
|---|------|---------|---------|
| T3-1 | ToolCall.validated/validation_id 始终 false/None | 删除这两个字段，guard_check 已独立实现安全检查 | `brain-core/src/types.rs` |
| T3-2 | confidence 硬编码 0.85 | 基于工具调用成功率和 LLM 调用次数简单计算 | `brain-main/src/main_brain.rs` |

### T4 — 低优先级（延后）

| # | 问题 |
|---|------|
| T4-1 | api_server 全局 Mutex 串行化（需架构重构） |
| T4-2 | history() 无外部调用 |
| T4-3 | sources 永远为空 Vec |
| T4-4 | max_context_tokens 硬编码 200K |
| T4-5 | checker/prompts/error 模块过度暴露 |
| T4-6 | store_rich_record 等方法无生产调用 |
| T4-7 | short_term 模块仅迁移用 |
| T4-8 | 其他低优先级项 |

## 验证方案

每完成一个分组后运行：
```bash
cd rust && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test -p brain-core -p brain-llm -p brain-main -p brain-memory -p brain-eval -p ai-brain-cli -p brain-integration-tests
```
