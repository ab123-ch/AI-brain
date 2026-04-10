# AI Brain Agent — 项目生命周期总览

> 更新日期：2026-04-07
> 项目路径：`/Users/chenh/RustObject/claw-code-parity`

---

## 阶段总览

```
开发设计 ████████████████████ 100%  ✅ 已完成
开    发 ████████████████████ 100%  ✅ 已完成
联    调 ████████████████████ 100%  ✅ 已完成
提    测 ░░░░░░░░░░░░░░░░░░░░   0%  🔵 当前阶段
验    收 ░░░░░░░░░░░░░░░░░░░░   0%  ⬜ 未开始
上    线 ░░░░░░░░░░░░░░░░░░░░   0%  ⬜ 未开始
```

---

## 一、开发设计 — ✅ 100%

### 交付物

| 文档 | 路径 | 内容 |
|------|------|------|
| 架构设计 | `docs/architecture/ai-brain-agent-design.md` | 6 副脑 + 三通道总线 + 记忆金字塔 |
| 流转示例 | `docs/architecture/example-holiday-query-flow.md` | 节假日查询完整消息流转 |
| 接口规约 | `docs/architecture/interface-specification.md` | BrainAgent trait / 消息类型 / API |
| 开发计划 | `docs/plans/development-plan.md` | 9 Phase 分阶段计划 |
| Phase 9 设计 | `docs/plans/2026-04-06-phase9-evolution-design.md` | 进化机制详细设计 |

### 关键设计决策

| # | 决策 | 理由 |
|---|------|------|
| 1 | 纯文件存储，不引入向量数据库 | 初期关键词召回足够，降低依赖 |
| 2 | 记忆永不删除，衰减只影响召回行为 | 保留完整历史，避免信息丢失 |
| 3 | 快思考(规则 ~10ms) + 慢思考(LLM ~1-5s) 双层 | 平衡速度与质量 |
| 4 | 评估脑无状态，执行后自毁 | 避免评估结果影响后续判断 |
| 5 | Rust 直调 GLM API（OpenAI 兼容），去掉 Python 依赖 | 简化部署，减少通信开销 |

---

## 二、开发 — ✅ 100%

### Crate 清单

| Crate | 职责 | 源文件 | 测试数 |
|-------|------|--------|--------|
| `brain-core` | 公共类型 + BrainAgent trait + 配置 | 4 | 4 |
| `brain-bus` | 三通道消息总线（broadcast/collaboration/result） | 3 | 11 |
| `brain-sensory` | 感知脑（唯一入口，LLM 解析 + 环境注入） | 3 | 5 |
| `brain-master` | 主脑（裁判 + 调度 + 权重引擎） | 3 | 11 |
| `brain-memory` | 记忆脑（L0-L3 总结金字塔 + 巩固引擎） | 9 | 33 |
| `brain-reasoning` | 推理脑（经验路径库 + 快/慢思考） | 4 | 20 |
| `brain-motor` | 执行脑（工具注册 + 选择） | 3 | 21 |
| `brain-validation` | 校验脑（安全校验 + 真实性校验） | 4 | 25 |
| `brain-evaluation` | 评估脑（上下文健康度 + 瘦身指令） | 3 | 16 |
| `brain-llm` | LLM 层（OpenAI 兼容 HTTP 客户端） | 4 | 14 |
| `brain-evolution` | 进化机制（注册中心 + 休眠 + 模板 + 建议） | 7 | 29 |
| `brain-integration-tests` | 跨 crate 集成测试 | 3 | 9 |
| `ai-brain-cli` | CLI + HTTP API + 编排器 | 4 | 2 |
| | **合计** | **59 文件 / ~9600 行** | **~200** |

### 按 Phase 完成记录

| Phase | 内容 | 完成日期 | 测试 |
|-------|------|----------|------|
| 1 | brain-core + brain-bus | 2026-04-04 | 15 |
| 2 | brain-sensory + brain-master + 集成测试 | 2026-04-04 | 22 |
| 3 | brain-memory（L0-L3 总结金字塔） | 2026-04-04 | 33 |
| 4 | brain-reasoning（经验路径库） | 2026-04-04 | 20 |
| 5 | brain-motor + brain-validation | 2026-04-04 | 46 |
| 6 | brain-evaluation + 权重进化引擎 | 2026-04-05 | 27 |
| 7 | brain-llm（Rust 直调 GLM API） | 2026-04-05 | 14 |
| 8 | ai-brain-cli + HTTP API | 2026-04-06 | 20+ |
| 9 | brain-evolution（注册中心/休眠/模板） | 2026-04-06 | 29 |

---

## 三、联调 — 🔵 10%（当前阶段）

### 阶段目标

将 13 个独立 crate 串联成可运行的完整系统，接通真实 LLM，跑通端到端查询。

### 任务清单

#### Task 10.1: 推理脑接入真实 LLM

- **状态**: ✅ 已完成（Phase 7 已实现）
- **优先级**: P0
- **改动文件**: `reasoning_engine.rs`, `reasoning_brain.rs`, `Cargo.toml`

#### Task 10.2: 执行脑接入真实 LLM

- **状态**: ✅ 已完成（Phase 7 已实现）
- **优先级**: P0
- **改动文件**: `motor_brain.rs`, `Cargo.toml`

#### Task 10.3: 编排器注入记忆到 ThinkContext

- **状态**: ✅ 已完成
- **优先级**: P0
- **改动文件**: `orchestrator.rs` — `handle_slow_think_dispatch()` 中 `memory_brain.recall_for_context()`

#### Task 10.4: 配置系统 & 目录初始化

- **状态**: ✅ 已完成
- **优先级**: P0
- **改动文件**: `init.rs` — `init_environment()` + `init_file_logging()` + `print_first_run_guide()`

#### Task 10.5: REPL 多轮对话

- **状态**: ✅ 已完成
- **优先级**: P1
- **改动文件**: `repl.rs` — 完整 REPL 循环 + 特殊命令

#### Task 10.6: 端到端验证测试

- **状态**: ✅ 已完成 (2026-04-08)
- **优先级**: P1
- **改动文件**: 新建 `e2e_real_llm.rs` — 5 个 `#[ignore]` 真实 LLM 测试
- **修复**: 死锁 bug（`query()` 中 `master_state` 锁未释放导致 `check_auto_dormancy()` 死锁）

#### Task 10.7: 日志 & 可观测性

- **状态**: ✅ 已完成
- **优先级**: P2
- **改动文件**: `init.rs` — 文件日志写入 `~/.ai-brain/logs/brain-{date}.log`

#### Task 10.8: 错误处理 & 用户引导

- **状态**: ✅ 已完成
- **优先级**: P2
- **改动文件**: `orchestrator.rs` — 无 API Key 降级为回声模式 + `init.rs` 首次运行引导

### 联调完成标准

- [x] `ai-brain query "你好"` 返回真实 LLM 回答（回声模式已验证闭环）
- [x] 复杂问题触发慢思考（推理脑/执行脑调 LLM）
- [x] 连续两轮对话，第二轮感知第一轮（记忆脑存储+召回）
- [x] 全量 624 测试通过 + clippy 0 warnings
- [x] `~/.ai-brain/logs/` 有运行日志

### 关键修复

- **死锁修复**: `Orchestrator::query()` 持有 `master_state` 锁的同时调用 `check_auto_dormancy()` 导致不可重入死锁。修复：`run_once()` 完成后立即释放锁（用 block scope）。

---

## 四、提测 — ⬜ 0%

> 前置条件：联调完成

### 提测范围

| 测试类型 | 覆盖 |
|----------|------|
| 单元测试 | 各 crate 独立测试（已有 ~200） |
| 集成测试 | 多 crate 联动（最小闭环 + 多脑 + 降级） |
| 端到端测试 | 真实 LLM 全链路（Task 10.6 产出） |
| 性能基线 | 快思考 < 50ms，慢思考 < 5s |
| 异常测试 | LLM 超时/降级、总线满、磁盘满 |

### 提测检查清单

- [ ] `cargo test --workspace` 全绿
- [ ] `cargo clippy --workspace -- -D warnings` 无警告
- [ ] `cargo fmt --check` 格式正确
- [ ] 真实 LLM E2E 测试通过
- [ ] 多轮对话记忆持久化验证
- [ ] 异常降级（无 API Key / 网络断开）不 panic

---

## 五、验收 — ⬜ 0%

> 前置条件：提测通过

### 验收场景

| # | 场景 | 预期 | 状态 |
|---|------|------|------|
| 1 | `ai-brain query "今天有什么节日？"` | 返回准确答案，含推理脑+记忆脑参与 | ⬜ |
| 2 | `ai-brain query "帮我分析这段代码的性能问题"` | 触发慢思考，LLM 深度推理 | ⬜ |
| 3 | 连续 5 轮对话 | 上下文连贯，记忆脑存储+召回正常 | ⬜ |
| 4 | `ai-brain serve` + HTTP API | `POST /api/query` 返回 JSON 结果 | ⬜ |
| 5 | `ai-brain brain suggest` | 使用后频次检测触发建议 | ⬜ |
| 6 | `ai-brain brain dormant/wake` | 副脑休眠/唤醒正常 | ⬜ |
| 7 | `ai-brain status` / `weights` / `memory stats` | 管理命令正常输出 | ⬜ |
| 8 | 无 API Key 启动 | 回声模式 + 友好提示 | ⬜ |

---

## 六、上线 — ⬜ 0%

> 前置条件：验收通过

### 上线清单

- [ ] `cargo build --release` 编译通过
- [ ] 二进制文件 `ai-brain` 可独立运行（无开发环境依赖）
- [ ] 默认配置模板可一键生成
- [ ] README 包含安装/配置/使用说明
- [ ] 首次运行引导流程（目录创建 → API Key 配置 → 第一次查询）

### 后续迭代（非上线阻塞）

| 项目 | 触发条件 |
|------|----------|
| 向量数据库 | L3 记忆 > 10000 条，关键词召回率 < 70% |
| 流式输出 | 接入 Web UI 后 |
| MCP 协议 | 需要 Python 生态能力时 |
| 并发查询 | 多用户场景 |
| Web UI | CLI 体验稳定后 |
