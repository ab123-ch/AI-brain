# 记忆金字塔重构 — 会话交接文档

> 生成时间: 2026-05-25
> 当前进度: Task 1-16 全部完成

## 一、任务概述

将 `brain-memory` crate 从追加式存储重构为 **原生多人格 + 金字塔提炼式记忆系统**。

**设计文档:** `docs/plans/2026-05-22-memory-pyramid-redesign.md`
**实施计划:** `docs/plans/2026-05-22-memory-pyramid-impl.md`

## 二、架构概要

```
四层金字塔 (per-persona 隔离):
  L4 潜意识层  — 触发词 + 叙事（覆盖式更新，500字/50触发词上限）
  L3 经验抽象层 — 按任务类型汇总经验（全量重生成，每类型≤10条）
  L2 任务摘要池 — 按任务分类摘要（全量重生成，≤50条）
  L1 全量基座   — 原始对话 JSONL（永久保存）

人格系统: PersonaManager 注册表 + PersonaConfig + per-persona 目录
浓缩引擎: 4步全量重生成（L1→L2→L3→L4 + Profile/EvalInfo）
```

## 三、已完成 Task 详情

### Task 1: 核心类型定义 ✅
- **文件:** `rust/crates/brain-memory/src/pyramid_types.rs` — PyramidLayer, TaskType, TaskSummary, L1Ref, SummaryIndex, Experience, TypeExperience, SubconsciousData, PersonaProfile, EvalInfo 等全部类型
- **文件:** `rust/crates/brain-memory/src/persona_types.rs` — Persona, PersonaConfig, PersonaRegistry

### Task 2: 原生人格系统 ✅
- **文件:** `rust/crates/brain-memory/src/persona_manager.rs`
- CRUD: load_or_create, list, active, switch, create, delete
- build_persona_prompt(), analysis_interval(), eval_sensitivity()

### Task 3: Per-Persona 存储层 ✅
- **文件:** `rust/crates/brain-memory/src/pyramid_storage.rs`
- PyramidStorage: l1_dir/l2_dir/l3_dir/l4_path/profile_path/eval_info_path
- 文件操作: read_json, write_json, append_jsonl, read_jsonl, clean_dir

### Task 4: L1 Raw Pool ✅
- **文件:** `rust/crates/brain-memory/src/raw_pool.rs`
- RawPool: append_turn, read_session, list_sessions, session_to_json

### Task 5: L2 Summary Pool ✅
- **文件:** `rust/crates/brain-memory/src/summary_pool.rs`
- SummaryPool: regenerate(全量覆盖), load_all, load_index, find_by_tags, find_by_id

### Task 6: L3 Abstraction Layer ✅
- **文件:** `rust/crates/brain-memory/src/abstract_layer.rs`
- AbstractLayer: regenerate, load_type, load_all, load_index, load_injectable, find_by_keyword

### Task 7: L4 Subconscious Pool ✅
- **文件:** `rust/crates/brain-memory/src/subconscious_pool.rs`
- SubconsciousPool: regenerate(覆盖式), load, match_triggers, narrative, inject_text

### Task 8: Profile + EvalInfo ✅
- **文件:** `rust/crates/brain-memory/src/profile_eval.rs`
- ProfileStore: regenerate(100字上限), load, summary
- EvalInfoStore: regenerate(requirements+pitfalls+rules), load, inject_text

### Task 9: Prompt 重写 ✅
- **文件:** `rust/crates/brain-memory/src/prompts.rs` (末尾追加)
- 4个新 prompt: CONCENTRATION_STEP1/STEP2/STEP3/STEP4_PROMPT
- 4个 builder: build_concentration_step1/2/3/4_prompt()

### Task 10: 四步浓缩引擎 ✅
- **文件:** `rust/crates/brain-memory/src/concentration.rs`
- ConcentrationEngine: run() → step1_l1_to_l2 + step2_l2_to_l3 + step3_l3_to_l4 + step4_profile
- AnalysisLlm trait, MockLlm 测试, parse_json_response

### Task 11: 渐进式召回引擎 ✅
- **文件:** `rust/crates/brain-memory/src/progressive_recall.rs`
- ProgressiveRecall: auto_inject, build_inject_text, recall(L4→L3→L2→L1)

### Task 12: 人格命令集成 ✅
- **文件:** `rust/crates/ai-brain-cli/src/command/persona_cmd.rs`
- 注册到 `command/mod.rs` 的 build_full_registry()
- 子命令: list, switch, info, create, delete（handler 为 placeholder，TUI app.rs 处理实际逻辑）

### Task 13: PyramidMemoryBrain ✅
- **文件:** `rust/crates/brain-memory/src/pyramid_memory_brain.rs`
- PyramidMemoryBrain: 统一入口，组合所有金字塔组件
- 接口: store_turn, tick_and_should_analyze, concentrate, auto_inject, recall, switch_persona, stats

## 四、待执行 Task

### Task 14: Orchestrator 集成 ⬜
**文件:** `rust/crates/ai-brain-cli/src/orchestrator.rs`
**改动点:**
1. 创建 PyramidMemoryBrain 替代旧 MemoryBrain（或并行共存）
2. 启动注入: inject_memory_context 改用 PyramidMemoryBrain.auto_inject()
3. 人格 prompt 注入: 从 PersonaManager.build_persona_prompt() 获取
4. 评估脑接口: eval-info.json 替代旧 pitfall/evolution/eval_requirement
5. 人格切换: 新增 switch_persona() 方法
6. pending_analysis 路径: 适配 per-persona 目录
7. analysis_interval: 从 PersonaConfig 读取（替代硬编码 5）

**关键参考行:**
- orchestrator.rs 行 ~1830: create_sub_brains()
- 行 ~479: 启动注入
- 行 ~742: store_turns
- 行 ~1340: tick_and_should_analyze

### Task 15: 旧数据迁移 ✅
- **文件:** 新建 `rust/crates/brain-migration/` crate
  - `Cargo.toml` — 依赖 brain-core + brain-memory + chrono + serde + serde_json
  - `src/main.rs` — 迁移工具主程序 + 8 个单元测试
- **迁移策略:**
  1. L1: `sessions/*.jsonl` → `personas/default/pyramid/l1-raw/*.jsonl`（格式转换: TurnRecord/FlexibleTurn → RawTurn）
  2. L2/L3/L4: 不迁移，下次四步分析时 LLM 自动生成
  3. Profile: `memory/profile/user_profile.json` → `personas/default/profile.json`（100字摘要）
  4. EvalInfo: `memory/pitfall/` + `memory/evolution/` + `memory/eval-requirement/` → `personas/default/eval-info.json`（过滤 superseded）
  5. Persona Registry: 直接写入 `personas/registry.json`（含 default 人格）
- **特性:**
  - 幂等操作（已存在则跳过）
  - 灵活 JSONL 解析（兼容 TurnRecord 和通用 {role,content,timestamp} 格式）
  - superseded 数据过滤（不迁移已废弃的踩坑/规则）
  - 使用: `cargo run -p brain-migration [base_dir]`（默认 ~/.ai-brain/）

### Task 16: 清理旧代码 ✅
- **已删除文件（7个）:** short_term.rs, event_index.rs, task_summary.rs, index_layer.rs, consolidation.rs, guardian.rs, memory_iteration.rs
- **已禁用 pub mod（5个旧模块，依赖链断裂）:** analyzer, brain_state, importance, memory_brain, recall
- **orchestrator.rs:** 旧 AnalysisLlm trait 改为固有方法，旧守护线程代码注释
- **lib.rs:** 新旧模块清晰分组，含回滚注释

## 五、测试状态

- `cargo test -p brain-memory`: **167 tests passed**（旧模块测试已随文件删除）
- `cargo test -p brain-migration`: **8 tests passed**
- `cargo test -p brain-core`: **25 tests passed**
- `cargo test -p brain-eval`: **61 tests passed**
- `cargo test -p brain-main`: **24 tests passed**
- 新增测试分布:
  - pyramid_types: 10 tests
  - persona_types: 5 tests
  - persona_manager: 9 tests
  - pyramid_storage: 9 tests
  - raw_pool: 9 tests
  - summary_pool: 7 tests
  - abstract_layer: 7 tests
  - subconscious_pool: 11 tests
  - profile_eval: 10 tests
  - concentration: 8 tests (含 MockLlm 集成)
  - progressive_recall: 6 tests
  - pyramid_memory_brain: 10 tests
  - persona_cmd: 7 tests
  - prompts (新增): 4 tests

## 六、注意事项

1. **旧代码清理完成**: Task 16 已删除 7 个旧模块文件，禁用 5 个依赖链断裂模块。lib.rs 含回滚注释。
2. **保留的旧模块**: archive, eval_requirement, evolution, pending_analysis, pitfall, raw_layer, storage, subconscious, summary, user_profile（无交叉依赖，可编译）
3. **旧 clippy 警告**: 保留的旧模块（storage.rs, subconscious.rs, summary.rs 等）仍有预存 clippy 警告。
4. **v2_integration_test 预存错误**: `ai-brain-cli/tests/v2_integration_test.rs` 有预存编译错误（与本次重构无关）。
5. **rusty-claude-cli 预存错误**: `ContentBlock::Thinking` match 不完整（与本次重构无关）。
