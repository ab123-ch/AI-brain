# AI Brain v2 — 全面问题修复计划

## 背景

全面排查发现 v2 架构骨架已搭好，但存在大量调用链断裂、空实现、配置未接入等问题。
核心原因是编排器没有把已有的真实实现连接起来。

## 修复任务分组（按优先级）

### P0 — 调用链断裂（核心功能不工作）

| # | 问题 | 修复方案 | 涉及文件 | 状态 |
|---|------|---------|---------|------|
| P0-1 | 生产用 StubToolExecutor，工具执行永远空 | 创建 RealToolExecutor 桥接 tools crate 的 execute_tool | `brain-core/src/tool_executor.rs`, `ai-brain-cli/src/real_tool_executor.rs` | [x] |
| P0-2 | register_tools() 从未调用 | 编排器初始化后调用 register_tools()，传入 mvp_tool_specs 转换后的定义 | `ai-brain-cli/src/orchestrator.rs`, `ai-brain-cli/src/real_tool_executor.rs` | [x] |
| P0-3 | BrainConfig 不读配置文件 | BrainConfig 添加 load_default() 方法，从 config.toml 加载 | `brain-core/src/config.rs`, `ai-brain-cli/src/orchestrator.rs` | [x] |
| P0-4 | process_input_streaming 不写回历史 | streaming 路径返回 oneshot receiver + commit_streaming_result() | `brain-main/src/main_brain.rs` | [x] |

### P1 — 对话历史管理

| # | 问题 | 修复方案 | 涉及文件 | 状态 |
|---|------|---------|---------|------|
| P1-1 | Tool/System/Evaluator 消息全映射为 User | ChatMessage 补充 Tool 角色，to_chat_messages 正确映射 | `brain-main/src/conversation.rs`, `brain-llm/src/provider.rs` | [x] |
| P1-2 | 工具调用中间消息未写入 ConversationHistory | tool_loop 返回完整消息链，process_input 写入历史 | `brain-main/src/tool_loop.rs`, `brain-main/src/main_brain.rs` | [x] |
| P1-3 | push_tool_result/push_evaluator 写了没调用 | 在编排器重试路径中调用 push_evaluator_to_history | `brain-main/src/main_brain.rs`, `ai-brain-cli/src/orchestrator.rs` | [x] |

### P2 — 编排器完善

| # | 问题 | 修复方案 | 涉及文件 | 状态 |
|---|------|---------|---------|------|
| P2-1 | streaming 路径评估不通过不重试 | 添加异步重试逻辑 | `ai-brain-cli/src/orchestrator.rs` | [x] |
| P2-2 | shutdown() 空操作 | 添加 shutting_down 标志 + Drop 调用 | `ai-brain-cli/src/orchestrator.rs` | [x] |
| P2-3 | eval.enabled 配置未检查 | query 中检查配置跳过评估 | `ai-brain-cli/src/orchestrator.rs` | [x] |
| P2-4 | api_server query 持 Mutex 锁跨 await | 改用一次性获取结果释放锁 | `ai-brain-cli/src/api_server.rs` | [x] |
| P2-5 | status API 用文本切割获取数值 | 暴露结构化 status_structured() 方法 | `ai-brain-cli/src/api_server.rs`, `ai-brain-cli/src/orchestrator.rs` | [x] |

### P3 — 记忆脑接入

| # | 问题 | 修复方案 | 涉及文件 | 状态 |
|---|------|---------|---------|------|
| P3-1 | recall_for_context 未接入主脑 | 编排器在构建上下文时注入召回结果 | `ai-brain-cli/src/orchestrator.rs`, `brain-memory/src/memory_brain.rs` | [ ] |
| P3-2 | should_trigger_analysis() 未被调用 | 编排器使用 MemoryBrain 的判断而非自行计算 | `ai-brain-cli/src/orchestrator.rs` | [ ] |
| P3-3 | total_size_bytes 永远返回 0 | 计算真实存储大小 | `brain-memory/src/memory_brain.rs` | [ ] |
| P3-4 | 环境上下文全写死空串 | 从系统获取 cwd/git_branch/platform | `brain-memory/src/memory_brain.rs` | [ ] |
| P3-5 | L1→L2 session_file 硬编码 | 传入真实会话来源 | `brain-memory/src/consolidation.rs` | [ ] |
| P3-6 | compression.rs 完全孤立 | 接入 compression 模块 | `brain-memory/src/compression.rs`, `brain-memory/src/lib.rs` | [ ] |

### P4 — 评估脑完善

| # | 问题 | 修复方案 | 涉及文件 | 状态 |
|---|------|---------|---------|------|
| P4-1 | 快速预检缺"已知失败模式"检测 | 添加基于 EvolutionRule 的模式匹配 | `brain-eval/src/checker.rs` | 延后 |
| P4-2 | 快速预检缺"事实正确性"预检 | 添加基本事实核验规则 | `brain-eval/src/checker.rs` | 延后 |
| P4-3 | 用户偏好/习惯未在快速预检中检查 | quick_check 接收完整 UserProfile | `brain-eval/src/eval_brain.rs`, `brain-eval/src/checker.rs` | 延后 |

### P5 — LLM 层修复

| # | 问题 | 修复方案 | 涉及文件 | 状态 |
|---|------|---------|---------|------|
| P5-1 | 默认 brain_models 用 v1 脑名 | 改为 main/memory/eval | `brain-llm/src/config.rs` | [x] |
| P5-2 | max_tokens/temperature 硬编码 | 添加 run_tool_loop_with_config 参数 | `brain-main/src/tool_loop.rs` | [x] |
| P5-3 | process_input_streaming 是假流式 | 接入 stream_complete | `brain-main/src/main_brain.rs` | 延后 |

### P6 — 配置清理

| # | 问题 | 修复方案 | 涉及文件 | 状态 |
|---|------|---------|---------|------|
| P6-1 | WeightConfig 整个模块未使用 | 删除 | `brain-core/src/config.rs` | [x] |
| P6-2 | PythonSection 未使用 | 删除 | `brain-core/src/config.rs` | [x] |
| P6-3 | model_main 死配置字段 | 删除 | `brain-core/src/config.rs` | [x] |
| P6-4 | memory_dir 未传递给 MemoryBrain | 传递 | `brain-core/src/config.rs`, `ai-brain-cli/src/orchestrator.rs` | [x] |
| P6-5 | 多个 threshold 字段未使用 | 已接入 context_warning/danger | `brain-core/src/config.rs` | [x] |

### P7 — 死代码清理

| # | 问题 | 修复方案 | 涉及文件 | 状态 |
|---|------|---------|---------|------|
| P7-1 | brain-core/evaluation.rs 全文件未使用 | 删除模块注册 | `brain-core/src/lib.rs` | [x] |
| P7-2 | brain-core/error.rs CoreError 未使用 | 保留（未来可能使用） | `brain-core/src/error.rs` | 保留 |
| P7-3 | MainBrainError 4 个变体未使用 | 保留（未来可能使用） | `brain-main/src/error.rs` | 保留 |
| P7-4 | FinishReason::StopSequence 未使用 | 删除 | `brain-llm/src/types.rs` | [x] |
| P7-5 | LlmError::BrainNotConfigured/JsonError 未使用 | 删除 | `brain-llm/src/error.rs` | [x] |
| P7-6 | stream.rs 全文件死代码 | 暂保留（P5-3 需要用到） | `brain-llm/src/stream.rs` | 保留 |
| P7-7 | BrainKind 枚举未使用 | 删除 | `brain-core/src/types.rs` | [x] |

### P8 — 测试增强

| # | 问题 | 修复方案 | 涉及文件 | 状态 |
|---|------|---------|---------|------|
| P8-1 | 集成测试全用 EchoLlm 无真实验证 | 添加 mock LLM 验证调用链 | `brain-integration-tests/tests/e2e.rs` | 延后 |
| P8-2 | 评估脑测试断言是空操作 | 构造记忆数据触发 LLM 评估路径 | `brain-integration-tests/tests/e2e.rs` | 延后 |

## 验证方案

每完成一个分组后运行：
```bash
cd rust && cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```
