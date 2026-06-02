# 智脑 Web UI 设计文档

> 日期: 2026-06-02
> 状态: 已确认
> 目标: 为智脑 AI 添加跨平台 Web 界面，支持 Windows/Mac/移动端浏览器访问

## 1. 背景

现有智脑只有 TUI 终端界面和 REST API，无 Web 前端。Web 界面的核心价值是**跨平台适配**——无论是在 Windows、Mac 还是移动端，打开浏览器即可使用。

### 使用场景
- **本地模式（优先）**: `ai-brain-cli --web` 启动，浏览器访问 `localhost:8080`
- **预留多用户**: 架构上预留认证和多用户隔离能力，MVP 不实现

## 2. 技术选型

| 层 | 选型 | 理由 |
|----|------|------|
| 后端 | Axum（已有依赖） | 无需新增依赖 |
| 前端 | 嵌入式单页 HTML+JS+CSS | 零 Node.js 依赖，`cargo build` 即包含前端 |
| 通信 | WebSocket（JSON 文本帧） | 天然映射 ProgressEvent，双向通信 |
| Markdown | marked.js + highlight.js | 轻量，内嵌为静态资源 |
| 部署 | 单二进制 | `include_str!` 编译时嵌入前端文件 |

## 3. 通信协议

### 3.1 客户端 → 服务端

| type | 字段 | 说明 |
|------|------|------|
| `query` | `{ input: string }` | 发送用户消息 |
| `cancel` | `{}` | 取消当前查询 |
| `ask_response` | `{ response: string }` | 回答 AskUserQuestion |
| `switch_persona` | `{ persona_id: string }` | 切换人格 |
| `new_session` | `{}` | 新建会话 |
| `switch_session` | `{ session_id: string }` | 切换会话 |
| `delete_session` | `{ session_id: string }` | 删除会话 |
| `toggle_thinking` | `{ visible: bool }` | 切换思考内容可见性 |

### 3.2 服务端 → 客户端

直接复用 `ProgressEvent` 枚举，通过 `WebProgressEvent` 适配层序列化（去除不可序列化字段如 `response_tx`）。

| type | 字段 | 说明 |
|------|------|------|
| `connecting` | `{ brain, model }` | 脑连接中 |
| `thinking` | `{ brain }` | 脑推理中 |
| `text_delta` | `{ text }` | 文本增量 |
| `thinking_delta` | `{ content }` | 思考内容增量 |
| `tool_start` | `{ brain, tool_name, input }` | 工具开始 |
| `tool_done` | `{ brain, tool_name, duration_ms, output_preview, is_error }` | 工具完成 |
| `memory_injected` | `{ count, preview }` | 记忆注入 |
| `memory_detail` | `{ memories }` | 记忆详情 |
| `evaluation_start` | `{}` | 评估开始 |
| `evaluation_result` | `{ passed, feedback }` | 评估结果 |
| `evaluation_detail` | `{ score, reports, instructions }` | 评估详情 |
| `evaluating` | `{}` | 评估中 |
| `llm_retry` | `{ attempt, max_attempts, error }` | LLM 重试 |
| `ask_user` | `{ question, options?, multi_select }` | 询问用户 |
| `done` | `{}` | 查询完成 |
| `session_list` | `{ sessions: [...] }` | 会话列表 |
| `session_switched` | `{ session_id, messages: [...] }` | 会话切换 |
| `persona_list` | `{ personas: [...], active_id }` | 人格列表 |
| `persona_switched` | `{ persona_id, name }` | 人格切换确认 |
| `error` | `{ message }` | 错误通知 |

### 3.3 协议细节
- 每帧一个完整 JSON 对象
- 30s 心跳 ping/pong
- `ProgressEvent → WebProgressEvent` 适配层处理 `AskUser.response_tx` 等不可序列化字段

## 4. 前端 UI 设计

### 4.1 布局（响应式）

**桌面 (>=768px)**:
```
┌──────────────────────────────────────┐
│  🧠 智脑   [人格▼]  [新建] [设置]   │
├──────┬───────────────────────────────┤
│ 会话1 │  对话区域                     │
│ 会话2 │                              │
│ 会话3 │  用户消息 / AI回复 / 工具调用  │
│      │  思考过程 / 系统消息           │
│      │                              │
│      │  ┌──────────────────────────┐ │
│      │  │ 输入消息...          [➤] │ │
│      │  └──────────────────────────┘ │
└──────┴───────────────────────────────┘
```

**移动端 (<768px)**:
```
┌──────────────────────┐
│ 🧠 智脑  [≡] [人格▼] │
├──────────────────────┤
│                      │
│  对话区域 (全屏)      │
│                      │
│  ┌──────────────────┐│
│  │ 输入...     [➤] ││
│  └──────────────────┘│
└──────────────────────┘
[≡] 点击展开侧滑会话列表
```

### 4.2 消息气泡类型

1. **用户消息**: 右对齐，深灰背景
2. **AI 回复**: 左对齐，Markdown 渲染（代码高亮）
3. **工具调用组**: 折叠面板，工具名+耗时+状态图标，点击展开显示输入/输出预览
4. **思考过程**: 可折叠区块，斜体文字，默认折叠，顶栏按钮全局切换
5. **系统消息**: 居中，小字灰色（人格切换、记忆注入、评估结果等）
6. **流式状态**: 打字机动画 + spinner "推理中..."

### 4.3 顶栏组件

- 左侧: Logo + "智脑"
- 中部: 当前人格下拉（显示人格名称和描述）
- 右侧: 新建会话按钮 + 设置按钮（移动端 hamburger 菜单）

### 4.4 技术实现

- 纯 CSS Grid/Flexbox 响应式布局
- CSS 变量定义主题色
- `marked.js` 做 Markdown 渲染
- `highlight.js` 代码语法高亮
- 原生 WebSocket API + 事件驱动 DOM 更新

## 5. 后端架构

### 5.1 文件结构

```
crates/ai-brain-cli/src/
├── web/                        # 新增 Web UI 模块
│   ├── mod.rs                  # 模块入口
│   ├── ws_handler.rs           # WebSocket 连接管理 + 消息路由
│   ├── session_manager.rs      # 多会话管理（内存 + 文件持久化）
│   ├── progress_adapter.rs     # ProgressEvent → WebEvent 适配
│   └── static/                 # 前端静态文件（include_str! 嵌入）
│       ├── index.html
│       ├── app.js
│       └── style.css
├── api_server.rs               # 修改：新增静态文件 + WebSocket 路由
└── main.rs                     # 修改：新增 --web 启动模式
```

### 5.2 WebSocket 处理流程

```
客户端连接 WS
  ↓
WsHandler::on_message(msg)
  ├── type=query        → Orchestrator::query_streaming() → 转发 ProgressEvent
  ├── type=cancel       → CancellationToken::cancel()
  ├── type=ask_response → oneshot::Sender.send()
  ├── type=switch_persona → PersonaManager::switch()
  ├── type=new_session   → SessionManager::create()
  ├── type=switch_session → SessionManager::switch() + 历史推送
  └── type=delete_session → SessionManager::delete()
```

### 5.3 多会话管理

- **SessionManager**: 维护 `HashMap<SessionId, WebSession>`
- **WebSession**: 包含对话历史 + 当前人格（共享 Orchestrator 的记忆脑）
- **持久化**: `~/.ai-brain/web-sessions/` 目录，每会话一个 JSON 文件
- MVP: 单 Orchestrator 多会话，共享记忆脑，独立对话历史

### 5.4 启动方式

```bash
ai-brain-cli --web              # 默认 0.0.0.0:8080
ai-brain-cli --web --port 3000  # 自定义端口
ai-brain-cli                    # 原有 TUI 模式不变
```

## 6. 错误处理

| 场景 | 处理策略 |
|------|---------|
| WebSocket 断开 | 客户端自动重连（指数退避，最多 5 次），重连后推送 `session_switched` 恢复 |
| LLM 调用超时 | 前端显示超时提示，`LlmRetry` 事件推送重试进度 |
| 工具调用失败 | `ToolDone.is_error=true`，前端红色标记，展开显示错误 |
| 多标签页连接 | MVP 允许，广播消息到所有连接 |
| 并发查询 | 同时只允许一个查询，前端禁用输入直到 `Done` |
| AskUserQuestion | 前端弹出选项面板，选择后发送 `ask_response` |

## 7. 主题设计

深色主题为主，CSS 变量方案:

```css
:root {
  --bg-primary: #1a1a2e;
  --bg-secondary: #16213e;
  --bg-chat: #0f3460;
  --bg-user-msg: #533483;
  --bg-ai-msg: #1a1a2e;
  --bg-tool: #1b2838;
  --text-primary: #e0e0e0;
  --text-secondary: #a0a0a0;
  --accent: #6c63ff;
  --success: #4caf50;
  --error: #f44336;
  --warning: #ff9800;
  --border: #2a2a4a;
}
```

## 8. MVP 范围

全部纳入 MVP:
- [x] 流式交互（WebSocket + ProgressEvent）
- [x] 多会话管理（新建/切换/删除/持久化）
- [x] 人格切换（下拉选择 + 实时生效）
- [x] 工具详情（折叠展开 + 输入/输出预览）

## 9. 延后项

- 多用户认证（预留架构位）
- 语音输入
- 文件上传/拖放
- 对话导出
- 明/暗主题切换
- 自定义主题色
