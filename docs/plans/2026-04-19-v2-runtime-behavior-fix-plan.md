# v2 运行时行为修复清单（第七轮）

> 审计时间: 2026-04-19
> 背景: 用户反馈主脑只会猜不会分析查证、多轮对话遗忘、system prompt 缺系统信息
> 审计方法: 逐文件分析主脑思考模式、上下文管理、记忆注入、系统信息
> 修复时间: 2026-04-19

---

## P0 (高) — 直接影响用户体验的核心问题

- [x] R-P0-1: **上下文超阈值自动重建** — `main_brain.rs:82-91` 改为调用 `history.truncate_to_recent(20)`，新增 `truncate_to_recent()` 方法
- [x] R-P0-2: **系统 prompt 增强思考框架** — `prompts.rs` 重写为"理解意图→信息评估→规划执行→执行验证→综合回答"五步框架 + 工具策略 + 回答准则
- [x] R-P0-3: **记忆召回改为独立 system 消息** — `orchestrator.rs` 两处路径（query + query_streaming）改用 `push_memory_context()` 替代拼接用户输入

---

## P1 (中) — 影响回答质量和信息准确度

- [x] R-P1-1: **系统 prompt 注入运行环境信息** — 新增 `build_environment_info()` 函数（OS、工作目录、日期），在 `build_messages()` 中追加到 system prompt
- [x] R-P1-2: **max_tokens 提升到 8192** — `main_brain.rs:41` 从 4096 改为 8192
- [x] R-P1-3: **tool_loop 验证环节** — 通过系统 prompt 指令实现（R-P0-2 的五步框架已包含验证步骤）

---

## P2 (低) — 代码质量和数据准确性

- [x] R-P2-1: **角色映射注释完善** — `conversation.rs` 补充详细注释说明 Tool/System/Evaluator→User 的原因和长期方案
- [x] R-P2-2: **token 估算对中文更友好** — `conversation.rs` 从 `chars/2` 改为 `chars*3/4`（混合场景折中估算）
- [x] R-P2-3: **四步分析后刷新记忆上下文** — `orchestrator.rs` 两处四步分析触发点新增刷新逻辑

---

## 不纳入本轮（已有延后标记）

| 项目 | 原因 |
|------|------|
| ConversationMessage 升级 blocks 模型 | S4 streaming 重构前置条件，改动面大 |
| REPL 命令 + API 端点补充 | 不影响核心功能 |
| streaming 路径历史不写回 | S4 范围 |

---

## 统计

| 级别 | 数量 | 已修复 |
|------|------|--------|
| P0 | 3 | 3 |
| P1 | 3 | 3 |
| P2 | 3 | 3 |
| **总计** | **9** | **9** |

## 验证

- `cargo check --workspace` ✅
- `cargo clippy --workspace --all-targets -- -D warnings` ✅
- `cargo test --workspace` — **633 tests passed**, 0 failed, 1 ignored
