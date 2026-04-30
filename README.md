# AI Brain v2 — 赛博智脑

基于 Rust 构建的 AI Agent 运行时，采用**一主二从**脑科学架构，具备跨会话记忆、自我进化、质量审核能力。

## 核心特性

- **一主二从架构** — 主脑（LLM + 工具调用循环）+ 记忆脑（后台分析）+ 评估脑（质量把关）
- **三层记忆系统** — L1 Raw → L2 Index → L3 Experience，支持跨会话持久化
- **四步自我分析** — 事实总结 → 记忆迭代 → 用户画像 → 踩坑分析
- **自进化规则引擎** — 自动从失败中学习，生成并应用进化规则
- **守护线程** — 自动归档旧记忆，管理重要性衰减/强化
- **TUI 交互界面** — crossterm + ratatui 终端 UI，支持粘贴、滚动、思考内容分离
- **20+ 内置工具** — 文件读写、搜索、Bash、Web、Agent 调度等
- **HTTP API 服务** — RESTful 接口，可嵌入其他应用

## 架构概览

```
                    ┌─────────────────────────────┐
                    │         Orchestrator         │
                    │       （编排调度中心）         │
                    └──────┬──────────┬────────────┘
                           │          │
              ┌────────────┘          └──────────────┐
              ▼                                      ▼
     ┌────────────────┐                    ┌──────────────────┐
     │   MainBrain    │                    │   EvalBrain      │
     │    （主脑）     │                    │   （评估脑）      │
     │                │                    │                  │
     │ • 五步思维框架  │◄── 质量反馈 ────│ • 快速规则预检    │
     │ • 工具调用循环  │                    │ • LLM 深度评估    │
     │ • 上下文管理   │                    │ • 六项检查        │
     │ • 8192 tokens │                    └──────────────────┘
     └────────────────┘
              │
              ▼
     ┌────────────────┐
     │  MemoryBrain   │
     │  （记忆脑）     │
     │                │
     │ • 三层记忆存储  │
     │ • 四步分析      │
     │ • 记忆迭代      │
     │ • 守护线程      │
     └────────────────┘
```

## 记忆架构

### 三层存储

| 层级 | 名称 | 存储内容 | 存储格式 |
|------|------|---------|---------|
| L1 | Raw | 全量原始对话轨迹 | JSONL（按会话） |
| L2 | Index | 短期记忆 + 关键词/事件/任务索引 | JSON |
| L3 | Experience | 经验包（从 L2 巩固提炼） | JSON |

### 四步分析流程

每 5 轮对话自动触发：

```
Step1 事实总结 ──→ Step0 记忆迭代 ──→ Step2 用户画像 ──→ Step3 踩坑分析
                     │                                      │
                     ▼                                      ▼
              OVERRIDE / COMPLEMENT              自进化规则生成
              REFINE / UNRELATED
```

- **OVERRIDE** — 新记忆完全取代旧记忆
- **COMPLEMENT** — 新记忆补充旧记忆
- **REFINE** — 精化已有记忆
- **UNRELATED** — 独立新记忆

### 记忆生命周期管理

- **重要性衰减** — 7 天未访问的记忆 importance ×0.9
- **重要性强化** — 召回时 importance +0.05
- **自动归档** — 守护线程检测到 8h+ 10 条未归档总结时，触发 L2→L1 归档
- **淘汰标记** — 30 天 + 低 importance → superseded

## Crate 结构

```
rust/crates/
├── brain-core/          # 公共类型（ConversationMessage, BrainState, ToolExecutor trait）
├── brain-llm/           # LLM Provider trait + OpenAI 兼容客户端
├── brain-main/          # v2 主脑（tool_loop + 五步思维 + 上下文管理）
├── brain-memory/        # 记忆脑（三层存储 + 四步分析 + 守护线程）
├── brain-eval/          # v2 评估脑（快速预检 + LLM 深度评估）
├── brain-sensory/       # 感知脑（输入预处理）
├── brain-reasoning/     # 推理脑
├── brain-motor/         # 执行脑
├── brain-evolution/     # 进化引擎
├── brain-hooks/         # Hook 系统
├── brain-bus/           # 消息总线
├── tools/               # 20+ 工具实现
├── ai-brain-cli/        # CLI + TUI + API Server + 编排器
└── brain-integration-tests/  # 集成测试
```

## 环境要求

| 依赖 | 版本要求 |
|------|---------|
| Rust | 2021 edition（建议 1.75+） |
| Tokio | 异步运行时（自动安装） |
| 操作系统 | macOS / Linux / Windows |
| LLM API | 兼容 OpenAI 格式的 API 服务 |

## 安装

```bash
# 克隆仓库
git clone <repo-url>
cd claw-code-parity

# 编译（debug 模式，首次较慢）
cd rust
cargo build

# 编译（release 模式，推荐）
cargo build --release
```

编译成功后，二进制文件位于：
- Debug: `target/debug/ai-brain`
- Release: `target/release/ai-brain`

## 配置

### 首次运行

直接启动 `ai-brain`，系统会自动创建配置目录和默认配置文件：

```bash
./target/release/ai-brain
```

自动创建的目录结构：
```
~/.ai-brain/
├── config.toml    # 配置文件
├── logs/          # 运行日志
├── memory/        # 记忆存储
├── sessions/      # 会话持久化
└── weights/       # 权重数据
```

### 配置文件

编辑 `~/.ai-brain/config.toml`：

```toml
[llm]
default_provider = "zhipu"
default_model = "glm-4.7"

[llm.providers.zhipu]
api_base = "https://open.bigmodel.cn/api/paas/v4"
# API Key 优先从环境变量读取
api_key_env = "ZHIPU_API_KEY"
# 也可以直接配置（不要提交到版本库）
# api_key = "your-api-key-here"

[llm.brain_models]
# 各脑可使用不同模型
sensory = "glm-4.7"
reasoning = "glm-5.1"
memory = "glm-4.7"
motor = "glm-5.1"
validation = "glm-5-turbo"

[llm.defaults]
max_tokens = 4096
temperature = 0.7
```

### API Key 配置

两种方式（环境变量优先）：

**方式一：环境变量（推荐）**

```bash
export ZHIPU_API_KEY="your-api-key"
./target/release/ai-brain
```

**方式二：写入配置文件**

编辑 `~/.ai-brain/config.toml`，设置 `api_key` 字段。

### 兼容其他 OpenAI 格式 API

以 OpenAI 官方为例：

```toml
[llm]
default_provider = "openai"
default_model = "gpt-4o"

[llm.providers.openai]
api_base = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
```

## 启动

### TUI 交互模式（默认）

```bash
# 终端中直接运行，自动进入 TUI 模式
ai-brain
```

### 单次查询

```bash
ai-brain query "你好，请介绍一下自己"
```

### HTTP API 服务

```bash
# 默认监听 127.0.0.1:3141
ai-brain serve

# 自定义地址
ai-brain serve --addr 0.0.0.0:8080
```

### 其他命令

```bash
ai-brain status           # 查看系统状态
ai-brain memory stats     # 记忆统计
ai-brain brain list       # 列出所有副脑
ai-brain brain templates  # 可用副脑模板
ai-brain weights          # 查看副脑权重
```

## TUI 操作指南

### 界面布局

```
┌──────────────────────────────────────┐
│                                      │
│            输出区域（弹性）           │
│                                      │
├──────────────────────────────────────┤
│ 状态栏                              │  ← 1 行
├──────────────────────────────────────┤
│                                      │
│            输入区域                  │  ← 3~10 行
│                                      │
└──────────────────────────────────────┘
```

### 快捷键

| 按键 | 功能 |
|------|------|
| **Enter** | 提交消息 |
| **Shift+Enter** | 插入换行 |
| **Ctrl+C / Esc** | 取消查询 / 退出 |
| **Ctrl+D** | 退出 |
| **Ctrl+E** | 切换详情模式（思考内容 + 记忆 + 评估） |
| **Ctrl+P** | 展开/折叠长粘贴 |
| **Ctrl+V** | 粘贴文本 |
| **Shift+↑/↓** | 滚动输出 |
| **↑/↓** | 翻阅输入历史 |
| **Tab** | 插入 4 空格 |
| **Home / Ctrl+A** | 光标到行首 |
| **End** | 光标到行尾 |

### 内置命令

在输入框中输入即可：

| 命令 | 功能 |
|------|------|
| `:help` | 显示帮助 |
| `:status` | 系统状态 |
| `:quit` | 退出并保存记忆 |

### 长文本粘贴

粘贴超过 200 字符或 5 行的文本时，输入区自动折叠为占位符显示：

```
[已粘贴 523 字符, 12 行 — Ctrl+P 展开]
```

按 **Ctrl+P** 可切换展开/折叠，**Enter** 直接提交完整内容。

## 评估脑

评估脑对主脑的输出进行质量审核，包含六项检查：

1. **重复踩坑** — 是否重复犯已知错误
2. **偏好违反** — 是否违反用户显性/隐性偏好
3. **已知失败模式** — 是否触发已记录的失败模式
4. **偷懒行为** — 是否包含 TODO/FIXME/省略号等偷懒标记
5. **事实正确性** — 回答是否基于事实
6. **规则合理性** — 自进化规则表述是否合理

评估结果通过 TUI 实时展示，评估不通过时主脑会收到反馈并改进。

## 工具列表

| 工具 | 功能 | 权限 |
|------|------|------|
| `bash` | 执行 shell 命令 | 完全访问 |
| `read_file` | 读取文件 | 只读 |
| `write_file` | 写入文件 | 工作区写入 |
| `edit_file` | 编辑文件内容 | 工作区写入 |
| `glob_search` | 文件名模式搜索 | 只读 |
| `grep_search` | 内容正则搜索 | 只读 |
| `WebFetch` | 获取 URL 内容 | 只读 |
| `WebSearch` | 网络搜索 | 只读 |
| `Agent` | 启动子 Agent | 完全访问 |
| `NotebookEdit` | Jupyter 笔记本编辑 | 工作区写入 |
| `EnterPlanMode` | 进入规划模式 | 工作区写入 |
| `ExitPlanMode` | 退出规划模式 | 工作区写入 |

## HTTP API

启动 `ai-brain serve` 后，可通过 REST API 交互：

```bash
# 查询
curl -X POST http://127.0.0.1:3141/api/query \
  -H "Content-Type: application/json" \
  -d '{"query": "你好"}'

# 系统状态
curl http://127.0.0.1:3141/api/status

# 记忆统计
curl http://127.0.0.1:3141/api/memory/stats

# 评估
curl -X POST http://127.0.0.1:3141/api/evaluate \
  -H "Content-Type: application/json" \
  -d '{"query": "测试", "response": "回答内容"}'
```

## 开发

```bash
# 格式化
cargo fmt

# Lint 检查
cargo clippy --workspace --all-targets -- -D warnings

# 运行测试
cargo test --workspace

# 排除集成测试（有外部依赖）
cargo test --workspace --exclude brain-integration-tests
```

## License

MIT
