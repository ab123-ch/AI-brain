# 系统架构设计 v2 — CLI / TUI / 编排层

> 三脑架构（主脑 + 记忆脑 + 评估脑）的系统基础设施设计

## 1. 系统启动流程

```
ai-brain CLI 启动
  │
  ├─ 加载配置 (~/.ai-brain/config.toml)
  │   └─ LLM 配置（provider/model/api_key）
  │
  ├─ 初始化目录结构
  │   ├─ ~/.ai-brain/logs/        — 运行日志
  │   ├─ ~/.ai-brain/memory/      — 记忆存储
  │   ├─ ~/.ai-brain/sessions/    — 会话持久化
  │   └─ ~/.ai-brain/config.toml  — 配置文件
  │
  ├─ 创建 LLM 客户端
  │   ├─ 主脑 LLM (reasoning model, 如 glm-5.1)
  │   ├─ 记忆脑 LLM (lighter model, 如 glm-4.7)
  │   └─ 评估脑 LLM (lighter model)
  │
  ├─ 初始化三个脑
  │   ├─ MainBrain::new(llm, tool_executor)
  │   ├─ MemoryBrain::new(memory_llm, storage)   ← 后台静默启动
  │   └─ EvalBrain::new(eval_llm)                 ← 后台静默启动
  │
  ├─ 记忆脑加载持久化数据
  │   ├─ L1 全量记忆 (JSONL)
  │   ├─ L2 索引摘要
  │   ├─ L3 经验包
  │   ├─ 用户画像
  │   ├─ 踩坑库
  │   └─ 自进化规则
  │
  └─ 进入 REPL 主循环
```

## 2. REPL 主循环

### 对齐 Claude Code 的交互模式

```
┌─────────────────────────────────────────┐
│  AI Brain v2                            │
│  模型: glm-5.1 | 记忆: 1.2GB | 会话: #3 │
└─────────────────────────────────────────┘
> 今天天气怎么样
  ⠋ 连接中... (glm-5.1)
  ⠸ 推理脑-推理中...
  推理脑 ✔ WebSearch("宁波 天气") (519ms)
  推理脑 ✔ WebFetch("weather.com/ningbo") (1.2s)

  宁波今天 22°C，晴，湿度 45%，适合外出。

  [参与: 主脑 | 耗时: 3.2s | tokens: 1.2k]

> 继续开发压缩功能吧
  ⠋ 推理脑-推理中...
  推理脑 ✔ read_file("memory_brain.rs") (45ms)
  推理脑 ✔ read_file("compression.rs") (38ms)
  💭 记忆注入: 5条 (用户偏好: 中文, 踩坑#3: TODO不算实现)
  推理脑 ✔ edit_file("compression.rs") (120ms)

  我看了 compression.rs 的实现，当前是规则压缩。
  按照 v2 设计，需要改为 LLM 缩句压缩...

  [参与: 主脑 | 评估: ✔通过 | 耗时: 8.5s | tokens: 3.4k]

>
```

### REPL 状态管理

```rust
struct ReplState {
    conversation_history: Vec<ConversationMessage>,  // 完整对话历史
    round_count: usize,                               // 用户输入轮次计数
    memory_rounds_since_summary: usize,               // 距上次总结的轮数
}
```

### 内置命令

| 命令 | 功能 |
|------|------|
| `/help` | 帮助信息 |
| `/status` | 系统状态（模型、记忆、token 用量） |
| `/memory` | 记忆统计（L1/L2/L3 条数、用户画像摘要） |
| `/model [name]` | 切换模型 |
| `/reset` | 重置当前会话（清空历史，保留记忆） |
| `/compress` | 手动触发记忆脑总结 |
| `/undo` | 撤销上一轮 |
| `/usage` | Token 用量统计 |

## 3. 终端进度显示（复用现有 terminal.rs）

### ProgressEvent 设计

```rust
pub enum ProgressEvent {
    Connecting { brain: String, model: String },
    Thinking { brain: String },
    ToolStart { brain: String, tool_name: String, input: String },    // 新增 input
    ToolDone { brain: String, tool_name: String, duration_ms: u64,
               output_preview: String, is_error: bool },               // 新增 output/error
    MemoryInjected { count: usize, preview: String },                  // 新增
    EvaluationStart,
    EvaluationResult { passed: bool, issues: Vec<String> },            // 新增
    LlmRetry { attempt: u32, max_attempts: u32, error: String },
    Done,
}
```

### 显示效果

```
推理脑 ✔ WebSearch("宁波 天气") (519ms)           ← 工具名+参数+耗时
推理脑 ✘ WebFetch("xxx") (2.1s) 超时             ← 失败红色显示
💭 记忆注入: 5条 — 用户偏好:中文, 踩坑#3:TODO    ← 记忆注入可见
✓ 评估通过                                        ← 评估结果可见
✗ 评估拦截: 又写了TODO (踩坑#3)                   ← 评估拦截可见
```

## 4. 会话持久化

### 存储格式 (JSONL)

每条消息一行，追加写入：

```jsonl
{"role":"user","content":"今天天气怎么样","timestamp":"2026-04-17T10:00:00Z"}
{"role":"assistant","blocks":[{"type":"text","text":"宁波22°C晴..."},{"type":"tool_use","id":"t1","name":"WebSearch","input":{"query":"宁波天气"}}],"timestamp":"..."}
{"role":"tool","tool_use_id":"t1","content":"宁波今天22°C...","duration_ms":519,"timestamp":"..."}
```

### 会话恢复

启动时检查 `sessions/current.jsonl`：
- 存在 → 加载历史，恢复对话
- 不存在 → 新会话

## 5. 配置系统（复用现有）

### config.toml 结构

```toml
[llm]
default_provider = "zhipu"
default_model = "glm-5.1"

[llm.providers.zhipu]
api_base = "https://open.bigmodel.cn/api/paas/v4"
api_key_env = "ZHIPU_API_KEY"

[llm.brain_models]
main = "glm-5.1"        # 主脑
memory = "glm-4.7"       # 记忆脑（可用轻量模型）
eval = "glm-4.7"         # 评估脑（可用轻量模型）

[llm.defaults]
max_tokens = 4096
temperature = 0.7

[memory]
summary_interval = 10    # 每 10 轮用户输入触发一次总结
context_threshold = 0.8   # 上下文使用率 80% 触发重建
preserve_recent_turns = 4 # 上下文重建时保留最近 4 轮

[eval]
enabled = true
auto_correct = true       # 自动纠正（不需要用户确认）
```

## 6. 日志系统

### 分级日志

```
RUST_LOG=info          → 正常运行日志
RUST_LOG=debug         → 包含 LLM 调用的 prompt/response
RUST_LOG=trace         → 包含完整的 HTTP 请求/响应
```

### 日志内容

| 级别 | 内容 |
|------|------|
| INFO | 工具调用（名称+参数+结果+耗时）、记忆触发、评估结果 |
| DEBUG | LLM 请求的 messages 摘要、LLM 响应全文、记忆脑四步分析详情 |
| TRACE | HTTP 请求/响应原始内容 |

## 7. API 服务（复用现有，调整接口）

```
POST /api/query          — 查询（返回流式或完整结果）
GET  /api/status         — 系统状态
GET  /api/memory/stats   — 记忆统计
POST /api/memory/compress — 手动触发压缩
GET  /api/memory/profile  — 用户画像
GET  /api/memory/pitfalls  — 踩坑库
```

## 8. Crate 结构

```
rust/crates/
├── brain-core/           ← 保留，清理旧类型
│   ├── types.rs          ← 清理 BroadcastMessage 等旧类型
│   ├── config.rs         ← 直接复用
│   ├── error.rs          ← 直接复用
│   ├── tool_executor.rs  ← 直接复用
│   ├── evaluation.rs     ← 直接复用
│   └── plan.rs           ← 直接复用 (StepResult, ToolCallRecord)
│
├── brain-llm/            ← 保留，100% 复用
│
├── brain-main/           ← 新建，主脑（替代 brain-reasoning + brain-master）
│   ├── main_brain.rs     ← 主脑核心
│   ├── tool_loop.rs      ← LLM↔工具循环（复用并增强）
│   ├── prompts.rs        ← 主脑系统提示词
│   └── conversation.rs   ← 对话历史管理
│
├── brain-memory/         ← 保留，重构
│   ├── memory_brain.rs   ← 重写为后台监听模式
│   ├── raw_layer.rs      ← 复用 L1 全量存储
│   ├── index_layer.rs    ← 复用 L2 索引
│   ├── experience_pack.rs← 复用 L3 经验包
│   ├── consolidation.rs  ← 重写为四步分析
│   ├── recall.rs         ← 复用三层召回
│   ├── user_profile.rs   ← 新建，用户画像
│   ├── pitfall.rs        ← 新建，踩坑库
│   └── evolution.rs      ← 新建，自进化规则
│
├── brain-eval/           ← 新建，评估脑
│   ├── eval_brain.rs     ← 评估脑核心
│   ├── prompts.rs        ← 评估提示词
│   └── checker.rs        ← 检查逻辑（基于踩坑库+用户画像）
│
├── ai-brain-cli/         ← 保留，重构
│   ├── main.rs           ← 复用 CLI 入口
│   ├── repl.rs           ← 重构 REPL 循环
│   ├── terminal.rs       ← 复用+增强进度显示
│   ├── orchestrator.rs   ← 重写编排器（简化）
│   ├── init.rs           ← 复用
│   └── api_server.rs     ← 复用+调整接口
│
└── brain-integration-tests/ ← 保留，重写测试
```

### 删除的 crate

- `brain-sensory/`  — 功能合并到主脑
- `brain-motor/`    — 功能合并到主脑
- `brain-validation/` — 功能合并到评估脑
- `brain-evolution/` — 功能合并到记忆脑
- `brain-bus/`      — 不再需要广播总线
- `brain-master/`   — 功能拆分到主脑和评估脑
- `brain-reasoning/` — 功能合并到主脑
