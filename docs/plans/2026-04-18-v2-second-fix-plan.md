# AI Brain v2 — 二次自检修复清单

## 背景

第一轮修复（P0-P8）完成了 22/30 项，P3（记忆脑接入）全部未做。
二次全面排查使用 4 agent 并行扫描全部 crate，发现 7 高 + 14 中 + 16 低 = 37 个唯一问题。
其中部分是第一轮已知但未修的（P3 组），部分是新发现的遗漏。

## 修复任务分组（按优先级）

### S1 — 核心功能断裂（必须修，从第一轮 P3 遗留 + 新发现）

| # | 问题 | 修复方案 | 涉及文件 | 来源 |
|---|------|---------|---------|------|
| S1-1 | recall_for_context 从未被编排器调用，三层召回完全不工作 | 编排器在构建主脑输入前调用 recall_for_context，将召回结果注入 system prompt 或上下文头部 | `ai-brain-cli/src/orchestrator.rs`, `brain-memory/src/memory_brain.rs` | 旧 P3-1 + 新 H1 |
| S1-2 | should_trigger_analysis 未被编排器调用，context_pressure 条件从未检查 | 编排器改用 MemoryBrain::should_trigger_analysis() 替代自研间隔逻辑，传入真实 context_pressure | `ai-brain-cli/src/orchestrator.rs`, `brain-memory/src/memory_brain.rs` | 旧 P3-2 + 新 H2 |
| S1-3 | 流式路径不写评估反馈到主脑历史 | query_streaming 重试循环中补充 push_evaluator_to_history 调用 | `ai-brain-cli/src/orchestrator.rs` | 新 H7 |
| S1-4 | on_round_complete 返回值恒为 false 且类型误导 | 修改 on_round_complete 在触发四步分析时返回 Ok(true)，删除硬编码 Ok(false) | `brain-memory/src/memory_brain.rs` | 新 M3 |
| S1-5 | 流式路径 record_and_maybe_trigger_analysis 中 on_round_complete 错误被忽略 | 改 `let _` 为 `if let Err(e)` 并打印 warn 日志 | `ai-brain-cli/src/orchestrator.rs` | 新 M3 |
| S1-6 | max_keywords 配置未传递给 MemoryBrainConfig | 编排器构造 MemoryBrainConfig 时传入 config.memory.max_keywords | `ai-brain-cli/src/orchestrator.rs` | 新 M7 |

### S2 — 死代码清理（纯删除，低风险）

| # | 问题 | 修复方案 | 涉及文件 | 来源 |
|---|------|---------|---------|------|
| S2-1 | plan.rs 175 行死代码（TaskPlan 等 8 个类型零引用） | 删除 plan.rs，移除 lib.rs 中 mod plan，清理 memory_types.rs 中的 RoundRecord.step_result 引用 | `brain-core/src/plan.rs`, `brain-core/src/lib.rs`, `brain-core/src/memory_types.rs` | 新 H4 |
| S2-2 | compression.rs 442 行死代码（ContextBuilder 等旧架构遗留） | 删除 compression.rs，移除 lib.rs 中 pub mod compression | `brain-memory/src/compression.rs`, `brain-memory/src/lib.rs` | 旧 P3-6 + 新 H5 |
| S2-3 | fact_extract.rs 352 行死代码（旧 Phase 2 事实提取） | 删除 fact_extract.rs，移除 lib.rs 中 pub mod fact_extract | `brain-memory/src/fact_extract.rs`, `brain-memory/src/lib.rs` | 新 H6 |
| S2-4 | memory_types.rs 中 8 个旧架构类型未使用 | 清理 InjectedContext/MemoryInjection/InjectionContent/RoundRecord/Phase/StepSummary/MemoryScore/injection_tier | `brain-core/src/memory_types.rs`, `brain-memory/src/prompts.rs`（清理 COMPRESSION_PROMPT 等） | 新 M5 |
| S2-5 | types.rs 中 Weight/BrainId/KnowledgeSource 4 变体未使用 | 删除 Weight 结构体及方法，删除 BrainId，精简 KnowledgeSource 为只保留 Memory 变体或标记 #[allow] | `brain-core/src/types.rs` | 新 M4 |
| S2-6 | brain-llm 中 BrainSection/ChatRequest.stream 字段死代码 | 删除 LlmConfig.brain 字段，删除 ChatRequest.stream 字段 | `brain-llm/src/config.rs`, `brain-llm/src/provider.rs` | 新 M14 + L16 |

### S3 — 中等优先级修复（设计完善）

| # | 问题 | 修复方案 | 涉及文件 | 来源 |
|---|------|---------|---------|------|
| S3-1 | context overflow 只打 warn 不处理 | 主脑检测到溢出时设置 needs_context_rebuild=true，编排器在调用前检查并触发重建 | `brain-main/src/main_brain.rs`, `ai-brain-cli/src/orchestrator.rs` | 新 M1 |
| S3-2 | sources 永远为空 | tool_loop 中收集实际知识来源（工具返回、记忆召回），填充 sources | `brain-main/src/main_brain.rs`, `brain-main/src/tool_loop.rs` | 新 M2 |
| S3-3 | total_size_bytes 永远返回 0 | 遍历 base_dir 下所有文件计算真实大小 | `brain-memory/src/memory_brain.rs` | 旧 P3-3 |
| S3-4 | 环境上下文全写死空串 | 从 std::env 获取 cwd、git branch、platform | `brain-memory/src/memory_brain.rs` | 旧 P3-4 |
| S3-5 | ThresholdConfig 3 个字段未使用 | 在对应位置接入 consolidation_importance、memory_recall_min_importance、context_warning_threshold | `brain-core/src/config.rs`, `brain-memory/src/recall.rs`, `brain-main/src/conversation.rs` | 新 M8 |
| S3-6 | run_tool_loop_with_config 无外部调用 | process_input 改为调用 run_tool_loop_with_config，传入 config 中的 max_tokens/temperature | `brain-main/src/main_brain.rs`, `brain-main/src/tool_loop.rs` | 新 M13 |
| S3-7 | stream_incremental 完整实现无调用者 | 标记 #[allow(dead_code)] 保留（给 streaming 重构用），或在 S4 中接入 | `brain-llm/src/provider.rs`, `brain-llm/src/stream.rs` | 新 M6 |

### S4 — Streaming 路径重构（需架构决策，延后）

| # | 问题 | 修复方案 | 涉及文件 | 来源 |
|---|------|---------|---------|------|
| S4-1 | 编排器 query_streaming 实际调用同步 process_input | 重构为调用 process_input_streaming，接入真流式 | `ai-brain-cli/src/orchestrator.rs`, `brain-main/src/main_brain.rs` | 旧 P5-3 + 新 H3 |
| S4-2 | streaming 路径丢失工具中间消息历史 | commit_streaming_result 改为写入完整消息链（含 tool call/result） | `brain-main/src/main_brain.rs` | 新 H3 |
| S4-3 | api_server 所有请求串行化 | 改用 RwLock 或取消外层 Mutex，仅对内部可变状态加锁 | `ai-brain-cli/src/api_server.rs`, `ai-brain-cli/src/orchestrator.rs` | 新 M11 |

### S5 — 低优先级清理（不阻塞功能）

| # | 问题 | 修复方案 | 涉及文件 | 来源 |
|---|------|---------|---------|------|
| S5-1 | confidence 硬编码 0.85 | 改为基于 tool_loop 结果动态计算 | `brain-main/src/main_brain.rs` | L1 |
| S5-2 | build_context_rebuild_prompt 无调用点 | 删除或接入 clear_and_rebuild | `brain-main/src/prompts.rs` | L2 |
| S5-3 | is_context_warning 无调用点 | 删除 | `brain-main/src/conversation.rs` | L3 |
| S5-4 | turn_usage 无调用点 | 删除 | `brain-main/src/conversation.rs` | L4 |
| S5-5 | System 分支死代码（重复匹配） | 删除 match 中 System 空分支 | `brain-main/src/main_brain.rs` | L5 |
| S5-6 | brain-main 4 个 pub mod 无外部引用 | 改为 pub(crate) mod 或保持（其他 crate 可能未来使用） | `brain-main/src/lib.rs` | L6 |
| S5-7 | EvalError::ResponseParseError 未构造 | 删除变体 | `brain-eval/src/error.rs` | L7 |
| S5-8 | checker 3 个 pub fn 无外部调用 | 改为 pub(crate) | `brain-eval/src/checker.rs` | L8 |
| S5-9 | IssueCategory 导出但无外部引用 | 保留（枚举变体可能通过模式匹配间接使用） | `brain-eval/src/lib.rs` | L10 |
| S5-10 | total_size_bytes 硬编码为 0 | 与 S3-3 合并处理 | `memory_brain.rs:383` | L12 |
| S5-11 | tags_index_path 无调用方 | 删除 | `storage.rs:83` | L13 |
| S5-12 | Orchestrator.llm 字段 dead_code | 保留标注 #[allow(dead_code)] | `orchestrator.rs:83` | L15 |

### S6 — 工具 stub 补充（逐个迭代，延后）

| # | 问题 | 修复方案 | 涉及文件 | 来源 |
|---|------|---------|---------|------|
| S6-1 | TaskGet/TaskList/TaskUpdate/TaskStop/TaskOutput 全是 stub | 接入文件持久化的任务系统 | `tools/src/lib.rs` | 新 M12 |
| S6-2 | CronCreate/CronDelete/CronList 全是 stub | 接入定时调度 | `tools/src/lib.rs` | 新 M12 |
| S6-3 | LSP/ListMcpResources/ReadMcpResource/MCP 全是 stub | 接入真实 LSP/MCP 客户端 | `tools/src/lib.rs` | 新 M12 |
| S6-4 | AskUserQuestion/SendMessage/EnterPlanMode/ExitPlanMode 全是 stub | 需要与 CLI REPL 交互回路集成 | `tools/src/lib.rs` | 新 M12 |

## 统计

| 分组 | 数量 | 性质 | 风险 |
|------|------|------|------|
| S1 核心断裂 | 6 | 必须修 | 中（改编排器核心逻辑） |
| S2 死代码清理 | 6 | 纯删除 | 低（删了不用的代码） |
| S3 设计完善 | 7 | 功能补全 | 中 |
| S4 Streaming 重构 | 3 | 架构决策 | 高（需设计评审） |
| S5 低优先级清理 | 12 | 代码卫生 | 低 |
| S6 工具 stub | 4 | 逐个迭代 | 低 |
| **合计** | **38** | | |

## 验证方案

每完成一个分组后运行：
```bash
cd rust && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

## 建议执行顺序

1. **S2（死代码清理）** — 先删除 ~1119 行死代码，减少干扰
2. **S1（核心断裂）** — 修复 3 个调用链断裂 + 3 个接入问题
3. **S3（设计完善）** — 补全 config 消费、来源追踪等
4. **S5（低优先级清理）** — 打扫卫生
5. S4、S6 延后
