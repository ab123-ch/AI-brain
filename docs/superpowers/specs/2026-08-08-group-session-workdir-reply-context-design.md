# 群会话固定工作目录与回复引用上下文设计

## 背景

当前 Web 群会话由 `ai-brain-cli` 的协作子系统管理：`collaboration_rooms` 保存房间状态，`room_events` 是顺序追加的公共消息池，`room_event_recipients` 保存用户消息的定向收件人，`member_inbox_items` 驱动实例领取任务。实例领取任务后，协作运行时构建并持久化 `ContextSnapshot`，再 fork 独立的 `MainBrain` 执行。

现有实现存在两个缺口：

1. `collaboration_rooms` 虽有未接入运行链的逻辑 `workspace_id`，但没有可恢复的文件系统工作目录。工具执行仍可能依赖进程全局 cwd，无法支持多个群并发使用不同目录。
2. `room_events` 已有实例回复使用的 `parent_event_id`，但用户发帖协议不能指定回复目标；默认三条用户消息窗口也不能保证较早的被引用消息进入目标实例上下文。

## 目标

- 每个群会话持久化一个独立工作目录。
- 新群默认使用服务启动时捕获的目录；用户可显式修改为任意已存在目录。
- 页面刷新、会话切换、服务重启后恢复该群原目录，不随进程 cwd 漂移。
- 每条用户消息在提交时冻结工作目录；设置变更只影响之后发送的消息。
- 用户可以回复当前群内任意有效用户或实例消息。
- 回复实例消息时自动选择原发送实例，同时允许追加其他收件实例。
- 所有实际收件实例都获得同一条权威引用信息，未选中实例不被唤醒。
- 引用即使超出普通历史窗口，也必须作为可追溯上下文进入实例运行。
- 多个群和多个实例并行执行时不修改进程全局 cwd。

## 非目标

- 不自动创建用户输入的目录。
- 不在目录失效时自动回退到服务启动目录。
- 不自动唤醒休眠或归档实例。
- 不绕过现有工具权限、sandbox 权限或绝对路径限制。
- 不增加全文消息搜索；较早消息通过有界分页加载后回复。
- 不改变现有显式 `@` 定向唤醒语义。
- 不把消息正文复制到新的引用表。

## 已确认的产品语义

### 工作目录

- 新群的初始目录是服务进程启动时捕获并规范化的目录。
- 用户可以输入绝对路径；相对路径以捕获的服务启动目录为基准解析。
- 数据库存储规范化绝对路径。
- 每条用户事件冻结提交时的目录。已经排队、运行、重试或恢复的任务继续使用自身冻结值。
- 修改目录允许与其他任务并发，只影响修改提交之后的新用户事件。

### 回复

- 可引用同群任意未失效的 `user_message` 或 `member_message`。
- 回复实例消息时，前端自动选择该实例；用户可以追加其他活跃实例。
- 回复用户消息时不自动选择实例。
- 引用上下文注入全部最终收件实例。
- 客户端只提交引用事件 ID；发送者、序号、正文和内容哈希均由服务端事件池解析。

## 总体架构

功能沿现有协作链路扩展，不绕过持久化任务和 `ContextSnapshot`：

```text
房间目录设置
    ↓ 持久化
collaboration_rooms.working_directory
    ↓ 发帖事务冻结
room_events.execution_working_directory + parent_event_id
    ↓ Inbox Claim
ClaimedInboxItem（目录 + 引用）
    ↓ 构建并持久化
ContextSnapshot + Task resolved_config
    ↓ 独立 MainBrain fork
显式 ToolExecutionContext.working_directory
```

协作仓储负责房间、消息、引用关系和冻结值；上下文构建负责模型可见内容；通用工具层负责 cwd 的实际执行语义；Web 层只维护交互状态，不成为权威数据源。

## 数据模型

### `collaboration_rooms`

新增：

```text
working_directory TEXT
```

新数据库直接建立非空字段；SQLite 升级先添加可空列、回填后再由仓储维持“规范化且非空”的运行时不变量。

`CollaborationRoomView` 同步新增 `working_directory: String`。该字段与现有逻辑 `workspace_id` 分离，避免把文件路径混入未来的多工作区标识语义。

房间更新使用现有 `version` 做乐观锁，并通过 outbox/房间快照通知客户端。

### `room_events`

新增：

```text
execution_working_directory TEXT
```

新写入的用户事件必须非空；旧数据迁移完成后 Claim 不接受缺少冻结目录的来源事件。

用户事件写入时保存房间当前目录。成员回复事件不启动新的实例任务，可继承父用户事件的目录以便审计，但 Claim 的权威目录始终来自其来源用户事件。

回复关系复用现有字段：

- 普通用户消息：`parent_event_id = NULL`，`conversation_root_event_id = event_id`。
- 回复用户消息：`parent_event_id = target.event_id`，`conversation_root_event_id = target.conversation_root_event_id`。
- 实例回复：`parent_event_id = source_user_event.event_id`，继续沿用该用户事件的会话根。

不新增正文副本。事件内容仍由 `room_events` 单一持久化来源提供。

### 事件视图

新增轻量引用投影 `RoomEventReferenceView`：

- `event_id`
- `sequence`
- `sender_kind`
- `sender_id`
- `sender_name`
- `kind`
- `content`
- `content_hash`
- `created_at`

`RoomEventView` 新增可选 `reply_reference`。仓储批量解析 `parent_event_id` 并填充它，使引用展示不依赖目标事件是否仍在最近 300 条房间快照中，也避免逐事件查询。

### Claim 与任务快照

`ClaimedInboxItem` 新增：

- `execution_working_directory`
- 可选的权威引用事件结构

持久任务 `resolved_config` 新增：

- `execution_working_directory`
- `reply_to_event_id`
- 引用来源 ID、序号和内容哈希

`ContextSnapshot` 仍是模型可见上下文的唯一冻结来源；`resolved_config` 保存目录和引用关系用于审计、恢复和一致性校验。

## 房间目录更新

WebSocket 新增 `update_room_working_directory` 命令：

- `working_directory`
- `expected_room_version`

服务端处理顺序：

1. 要求新增的 `ConfigureRoom` 能力。
2. 去除输入首尾空白并拒绝空值。
3. 绝对路径直接解析；相对路径基于服务启动目录解析。
4. 调用文件系统规范化，确认路径存在且为目录。
5. 在 Immediate 事务中校验房间版本并更新目录与版本。
6. 写入房间 changed outbox，提交后广播权威快照。

更新失败时事务不改变原目录。

新增 `ConfigureRoom` 到 owner 默认能力。升级现有数据库时必须更新本地 owner 的 capability JSON，不能只依赖 `ON CONFLICT DO NOTHING`。

## 发帖与回复事务

`PostRoomMessage` 新增向后兼容字段：

```text
reply_to_event_id: Option<String>
```

仓储发帖入口同步接收该字段。在现有收件人、容量、成员版本和房间版本校验之外，同一事务执行：

1. 读取房间当前 `working_directory`。
2. 如果存在回复目标，确认目标：
   - 属于当前房间；
   - `invalidated_at IS NULL`；
   - 类型是 `user_message` 或 `member_message`；
   - 序号早于待创建事件。
3. 分配新事件序号。
4. 写入正文、`parent_event_id`、会话根和冻结目录。
5. 写入去重后的收件人关联和 Direct Inbox。
6. 更新房间版本并写 outbox。

客户端不能提交引用正文或发送者信息。幂等重放返回首次创建的事件、冻结目录、引用关系和 Inbox，不按当前房间设置重新计算。

## 引用上下文

`knowledge-core::ContextBlockKind` 新增：

```text
ConversationReference
```

引用块是必需块，格式明确区分引用与当前输入，例如：

```text
[被回复引用]
发送者：智脑 A（member）
事件序号：42
正文：
原消息正文
```

引用块设置：

- 稳定 block ID：`reply-reference:<event_id>`
- `SourceRef.resource_id = event_id`
- `source_revision = sequence`
- `source_hash = content_hash`
- 沿用现有 `conversation.turn` 的来源信任策略，不由客户端提升 trust

`member_inputs_from_snapshot` 将 `ConversationReference` 放入成员 system context，使其作为“当前回复所针对的权威材料”出现，而不是伪装成一条新的普通历史轮次。

如果引用目标也落在最近三条用户消息和实例自身回复构成的普通历史中，普通历史投影跳过该事件，只保留必需引用块。当前输入仍保持单独的 `CurrentInput`，来源仍是本次用户事件。

引用内容不静默删除或截断。如果系统策略、引用和当前输入这些必需块超过上下文预算，构建在模型调用前失败并返回可行动错误。

## 显式工具执行上下文

在 `brain-core` 定义不可变 `ToolExecutionContext`，至少包含：

```text
working_directory: PathBuf
```

`MainBrain` 的隔离 fork 接收该上下文，`tool_loop` 在每次工具执行时显式传递。通用 `ToolExecutor`、生产 `RealToolExecutor`、群消息工具包装器和测试 executor 均按同一接口传递上下文。

`tools` 与 `runtime` 增加显式目录入口：

- shell/PowerShell/REPL 子进程设置 `current_dir(context.working_directory)`；
- read/write/edit 的相对路径从该目录解析；
- glob/grep 的缺省搜索根使用该目录；
- Config、计划状态和其他本地工作区文件从该目录解析；
- sandbox 配置加载与 workspace 根使用该目录；
- 本地插件进程若声明使用调用工作区，也从该目录启动；
- 远程 MCP 工具不隐式改写参数，除非其现有协议明确接收工作目录。

旧的一对一调用继续构造默认 `ToolExecutionContext`，其目录为原有服务/请求目录，以保持行为兼容。严禁用 `std::env::set_current_dir` 实现群目录切换。

协作成员运行从持久任务读取冻结目录，先验证目录仍存在且为目录，再创建隔离 `MainBrain`。因此多个群可以并发运行，各自工具调用不会共享可变 cwd。

现有 `GroupMessageToolExecutor` 与工作目录上下文正交：它继续限制 `read_group_messages` 的房间和序号上界，同时把执行上下文原样传给底层 executor。

## Web 交互

### 工作目录

房间标题区显示当前规范化目录，并提供设置操作。提交更新后等待服务端快照确认，不在客户端先行覆盖权威值。

错误以中文 toast/错误事件显示。版本冲突时请求最新房间快照，保留用户输入以便重新提交。

### 回复

每条有效用户/实例消息显示回复按钮。点击后：

- 保存 `reply_to_event_id`；
- 在输入框上方显示发送者和正文摘要；
- 提供取消按钮；
- 若发送者是活跃实例，自动加入收件人；
- 若实例休眠或归档，显示不可用提示，不自动唤醒；用户可先恢复实例或选择其他活跃实例。

用户仍可增加或取消其他收件人。发送载荷只包含引用 ID。发送成功、取消回复、切换群或删除当前引用目标时清理 composer 引用状态。

时间线使用 `reply_reference` 渲染引用摘要，因此页面重开后仍可恢复。引用用户消息时不自动选择实例。

### 较早消息加载

为兑现“同群任意有效消息均可引用”，WebSocket 增加有界的 `load_room_events_before` 请求，包含 `before_sequence` 和受服务端上限约束的 `limit`。仓储按序号倒序读取、响应按时间升序返回，并继续附带 `reply_reference`。

时间线顶部在仍有更早事件时显示“加载更早消息”。加载结果与当前事件按 `event_id` 去重后合并；用户可以逐页定位旧消息并点击回复。该接口只能读取当前活跃房间，不接受客户端指定其他房间。

## 错误处理与安全

### 目录错误

- 空路径：拒绝。
- 路径不存在：拒绝。
- 路径是文件：拒绝。
- 无法规范化：拒绝并返回文件系统原因。
- 房间版本冲突：不覆盖，返回冲突并刷新快照。
- 已保存目录后来被移动、删除或失去访问权限：相关任务在模型和工具执行前失败，不回退。

路径设置需要 `ConfigureRoom`。工作目录只改变相对路径和 workspace 根，不提升工具权限；现有 permission mode、sandbox 和危险操作审批仍然生效。

### 引用错误

- 目标不存在、跨房间、已失效或类型不支持：拒绝发帖。
- 引用实例不可用：不自动唤醒；若最终没有活跃收件人，沿用现有空收件人拒绝语义。
- 必需上下文超预算：模型调用次数为零，Claim/Inbox 记录可诊断失败；若恢复路径已有持久任务，则同步记录任务失败。
- 客户端伪造引用正文：协议不接收该字段，因此不存在信任路径。

## 数据迁移与兼容性

升级到下一 schema 版本时：

1. 为房间增加 `working_directory`。
2. 为事件增加 `execution_working_directory`。
3. 用启动时捕获目录回填没有目录的现有房间。
4. 用各房间回填后的目录补齐现有用户事件的冻结目录；已有成员回复可从其父用户事件继承或保持可推导。
5. 更新已有本地 owner 的能力集合，加入 `ConfigureRoom`。
6. 保留现有 `workspace_id`、事件关系、Inbox、delivery 和重试数据。

旧客户端省略 `reply_to_event_id` 时反序列化为 `None`。新快照增加的字段对忽略未知字段的旧前端/客户端保持兼容。旧的一对一会话不写群目录字段，也不改变历史恢复语义。

## 测试设计

### 仓储与迁移

- 新房间继承显式注入的启动目录。
- 修改目录后重新打开仓储，即使新的启动目录不同，仍恢复房间原目录。
- 消息 1 冻结旧目录，修改后消息 2 冻结新目录。
- 消息 1 的排队、重试和恢复仍使用旧目录。
- 缺失目录、文件路径、版本冲突和权限不足均不修改数据库。
- 旧 schema 升级后房间、事件和 owner 能力正确回填。
- 回复关系、会话根和轻量引用视图正确。
- 幂等重放不重新计算目录或引用。
- 跨房间、失效和非法类型引用被拒绝。

测试通过向仓储显式注入启动目录完成，不在并行测试中修改进程 cwd。

### 上下文与持久任务

- 引用早于最近三条用户消息时仍存在必需 `ConversationReference`。
- 引用同时出现在普通窗口时只注入一次。
- 引用块包含正确事件 ID、序号、发送者、正文和哈希。
- 当前输入与引用来源保持独立。
- 全部实际收件 Claim 获得相同引用，未选中实例没有 Inbox。
- `resolved_config` 保存冻结目录、回复关系和完整 `ContextSnapshot`。
- 必需块超预算时模型 provider 调用次数为零。

### 工具执行

- 两个临时目录并发执行 shell、读写文件、glob 和 grep，各自解析相对路径到自己的目录。
- sandbox/config 加载使用显式目录。
- 测试和生产实现均不调用 `set_current_dir`。
- 旧默认执行上下文保持现有一对一行为。
- 群消息只读工具包装器正确转发执行上下文。

### Web 协议与 UI

- 新旧 `post_room_message` JSON 均可反序列化。
- `update_room_working_directory` 的字段与版本语义正确。
- 回复实例消息自动选择发送实例并允许追加收件人。
- 回复用户消息不自动选择实例。
- 引用预览、取消、发送后清理和切换群清理正确。
- 目录快照和引用摘要在页面重开后恢复。
- 休眠/归档实例不会被隐式唤醒。
- 较早事件分页受当前房间和最大 limit 约束，合并时不重复或打乱事件。

### 验证门禁

从 `rust/` 执行：

```bash
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

同时运行现有 Web JavaScript 测试及新增的目录/回复纯函数测试。

## 验收标准

1. 两个群设置不同目录并并发调用相对路径工具时，各自只影响自己的目录。
2. 群 A 修改目录后，旧消息仍在旧目录执行，新消息在新目录执行。
3. 重启服务并重新打开群后，目录与引用预览保持不变。
4. 回复一条窗口外的旧消息时，所有实际收件实例的持久 `ContextSnapshot` 都包含唯一、可追溯的引用块。
5. 回复实例消息自动选择该实例，追加实例也获得引用，未选实例不运行。
6. 无效目录与非法引用均在持久化或模型调用前被拒绝，且不发生静默回退。
7. 旧客户端、旧房间、旧事件和一对一会话继续工作。
8. 用户可以通过有界分页加载较早消息并回复，不受初始 300 条快照窗口限制。
