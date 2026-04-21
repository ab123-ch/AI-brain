# v2 第三轮全量审计问题清单

> 审计时间: 2026-04-18
> 审计范围: brain-core, brain-llm, brain-main, brain-eval, brain-memory, ai-brain-cli, tools
> 审计维度: 死代码 / 空实现 / 设计不符
> 修复时间: 2026-04-18

---

## P0 (高) — 影响核心功能的断裂/错误

### 数据模型层
- [x] P0-3: `brain-core/error.rs` — CoreError/CoreResult 全项目零引用 → 已删除 error.rs + thiserror 依赖
- [ ] P0-1: `brain-core/types.rs` — ConversationMessage 用 content:String 而非 blocks:Vec<ContentBlock>（延后: S4 前置条件）
- [ ] P0-2: `brain-core/types.rs` — 缺少 ContentBlock 枚举 (Text/ToolUse/ToolResult)（延后: S4 前置条件）

### 主脑
- [x] P0-4+P0-5: 移除了 confidence/eval_passed/eval_issues/sources/needs_context_rebuild 死字段 → MainBrainOutput 简化为 {answer, usage}

### 评估脑
- [x] P0-6: EvalBrain 增加 progress_tx + emit_result → 评估开始/结束发送 ProgressEvent

### 记忆脑
- [x] P0-7: read_all_unconsolidated → 增加 .filter(|e| !e.consolidated)
- [x] P0-8: 巩固后标记 consolidated=true → ConsolidationEngine 调用 mark_consolidated + Storage.write_jsonl

### CLI/编排层
- [ ] P0-9: REPL 缺 5 个命令（延后: 不影响核心功能）
- [ ] P0-10: API 缺 3 个端点（延后: 不影响核心功能）
- [x] P0-11: 18 个 stub 工具从 mvp_tool_specs 移入 stub_tool_specs（#[allow(dead_code)] 保留代码）

---

## P1 (中) — 设计不符但可延后

### 缺失功能
- [ ] P1-1: 缺 retry_llm_call（3次重试10s间隔）
- [ ] P1-2: System prompt 缺 Memory Context 注入
- [ ] P1-3: max_tokens/llm_max_tokens 硬编码
- [ ] P1-4: KnownFailurePattern/InstructionIgnored 无快速预检
- [ ] P1-5: EvalIssue 缺 pitfall_ref 字段

### 设计偏差
- [ ] P1-6~P1-8: 结构体字段与设计文档偏差（当前字段足够用，延后扩展）

### 死字段/死路径
- [ ] P1-9: generate_and_persist_snapshot 零调用
- [ ] P1-10: ToolCall.validated/validation_id 死字段
- [ ] P1-11: MessageRole::Evaluator 在 brain-llm 无对应
- [ ] P1-12: query/query_streaming 记忆注入逻辑重复
- [ ] P1-13: stream_complete/stream_incremental 零调用 (S4)
- [x] P1-14: brain-llm/Cargo.toml brain-core 依赖零引用（保留: 未来 ContentBlock 共享需要）

---

## P2 (低) — 代码清洁度

### 已修复
- [x] P2-2: MemoryError::PathNotFound 删除
- [x] P2-5: max_keywords 配置字段修复
- [x] P2-6: brain-main 错误变体清理 (ToolError/GuardFailed/ContextOverflow)
- [x] P2-12: ai-brain-cli 移除 toml/thiserror 依赖
- [x] P2-15: brain-eval 模块可见性修正 (pub(crate))
- [x] P2-16: #[allow(dead_code)] 改为 _passed + #[serde(rename)]

### 待清理（延后）
- [ ] P2-1: event_index/short_term/task_summary 旧模块 ~720行
- [ ] P2-3: quick_check 零调用
- [ ] P2-4: resolve_source_refs 零调用
- [ ] P2-7: MainBrainOutput.sources 已移除
- [ ] P2-8: MainBrainOutput.needs_context_rebuild 已移除
- [ ] P2-9: ExperiencePack.files_modified_patterns/tools_used 永远为空
- [ ] P2-10: ExperiencePack.success_rate 硬编码 0.8
- [ ] P2-11: is_stop_word ~140行性能
- [ ] P2-13: serde_json 仅测试使用
- [ ] P2-14: chrono 仅测试使用（实际生产有使用，跳过）
- [ ] P2-17: dirs_home() 重复定义
- [ ] P2-18: init_file_logging 永远不生效
- [ ] P2-19: 流式路径结果完整重打 answer
- [ ] P2-20: MemoryStatsResponse 非结构化

---

## 统计

| 级别 | 总数 | 已修复 | 延后 |
|------|------|--------|------|
| P0 | 11 | 6 | 5 |
| P1 | 14 | 1 | 13 |
| P2 | 20 | 6 | 14 |
| **总计** | **45** | **13** | **32** |

### 验证状态
- clippy: 零警告
- test: 629 passed, 0 failed (brain 相关全通过)

---

## 第四轮审计 — 新增问题（2026-04-18）

### N-P0 (高) — 运行时 Bug

- [ ] N-P0-1: `brain-main/conversation.rs:133-143` — Tool role 消息缺少 tool_call_id，导致 API 400
- [ ] N-P0-2: `brain-main/main_brain.rs:142-148` — Tool 分支死代码（tool_result 用 User role）
- [ ] N-P0-3: `ai-brain-cli/api_server.rs:70-73` — 跨 await 持锁，串行化所有 API 请求

### N-P1 (中) — 编排层断裂

- [ ] N-P1-1: `brain-eval/eval_brain.rs:120` — set_progress_tx 零调用，评估进度从未发出
- [ ] N-P1-2: `brain-eval/eval_brain.rs:220-227` — emit_result 发空 issues
- [ ] N-P1-3: `brain-memory/memory_brain.rs:251` — get_brain_state 零调用，上下文重建未接入
- [ ] N-P1-4: `brain-memory/memory_brain.rs:296` — store_rich_record 零调用
- [ ] N-P1-5: `brain-memory/memory_brain.rs:322` — store_facts 零调用
- [ ] N-P1-6: `brain-memory/memory_brain.rs:418+428` — get_confirmed_facts/load_experience 零调用
- [ ] N-P1-7: `brain-main/main_brain.rs:177-242` — streaming 路径用硬编码 tool_loop 配置
- [ ] N-P1-8: `brain-main/main_brain.rs:180-242` — streaming 路径不写回工具调用历史
- [ ] N-P1-9: `tools/lib.rs:863` — "Brief" 幽灵别名无文档
- [ ] N-P1-10: `tools/lib.rs:2985-2991` — iso8601_now 返回 UNIX 时间戳非 ISO 格式
- [ ] N-P1-11: `tools/lib.rs:2178` — 硬编码绝对路径 /home/bellman/.codex/skills
- [ ] N-P1-12: `tools/lib.rs:3672 vs 2159` — .claw vs .clawd 命名不一致
- [ ] N-P1-13: `orchestrator.rs:83-84` — shutting_down 标志无实际行为
- [ ] N-P1-14: `orchestrator.rs:190+294` — query_count 在 eval 重试中重复计数
- [ ] N-P1-15: `real_tool_executor.rs:80-90` — mvp_tool_definitions 重复转换
- [ ] N-P1-16: `brain-core/config.rs:84-93` — max_keywords 死配置
- [ ] N-P1-17: `brain-core/config.rs:98-112` — max_retries 死配置
- [ ] N-P1-18: `brain-llm/error.rs:24` — IoError 死变体

### N-P2 (低) — 死方法/死依赖

- [ ] N-P2-1: `brain-memory/pitfall.rs:62` — load_by_category 零调用
- [ ] N-P2-2: `brain-memory/evolution.rs:69` — load_high_priority 零调用
- [ ] N-P2-3: `brain-memory/memory_brain.rs:368-387` — consolidate/run_gc 每次新建引擎
- [ ] N-P2-4: `brain-memory/memory_brain.rs:192-196` — on_round_complete 返回值未使用
- [ ] N-P2-5: `brain-main/tool_loop.rs:21-26` — ToolLoopResult 应 pub(crate)
- [ ] N-P2-6: `brain-main/main_brain.rs:149-151` — System 分支不可达
- [ ] N-P2-7: `brain-core/types.rs:7-12` — BrainContext 应在 brain-memory
- [ ] N-P2-8: `brain-core/memory_types.rs` — 单类型文件应合入 brain-memory
- [ ] N-P2-9: `brain-main/conversation.rs:60-66` — context_usage 注释不准确
- [ ] N-P2-10: `tools/lib.rs:3194-3196` — BriefStatus match 无差异化
- [ ] N-P2-11: `tools/lib.rs:3821` — PowerShell description 被丢弃
- [ ] N-P2-12: `tools/lib.rs:4030-4040` — parse_skill_description 不支持 Markdown
- [ ] N-P2-13: `brain-integration-tests/Cargo.toml` — tools 死依赖（引入 api/plugins/runtime）
- [ ] N-P2-14: `brain-integration-tests/Cargo.toml` — chrono/tracing/serde_json 死依赖
- [ ] N-P2-15: `brain-llm/Cargo.toml` — chrono 死依赖
- [ ] N-P2-16: `brain-main/Cargo.toml` — serde 死依赖
- [ ] N-P2-17: `brain-eval/Cargo.toml` — chrono 应在 dev-dependencies
- [ ] N-P2-18: `brain-memory/Cargo.toml` — tokio 应在 dev-dependencies

### 第四轮统计

| 级别 | 数量 |
|------|------|
| N-P0 | 3 |
| N-P1 | 18 |
| N-P2 | 18 |
| **新增总计** | **39** |
