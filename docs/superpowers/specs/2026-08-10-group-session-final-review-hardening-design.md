# 群会话目录与快照终审加固设计

## 背景

完整分支终审确认三个影响合并的问题：目录更新成功关联依赖客户端猜测 canonical 路径；房间版本与事件序号无法排序只改变成员/Inbox 的快照；Unix 目录失去访问权限时仅做 `metadata` 检查不足以在模型调用前失败。另有一个持久化边界：canonical 路径无法无损转成 UTF-8 时不得用替换字符写入数据库。

本加固不改变“相对路径以服务启动目录解析”、消息冻结目录、回复上下文或工具权限语义，只补齐成功确认、快照顺序和目录可用性合同。

## 目录更新成功确认

`update_room_working_directory` 增加客户端 `command_id`。服务端成功后向发起连接返回：

```json
{
  "type": "room_working_directory_accepted",
  "room_id": "room-1",
  "command_id": "command-1",
  "snapshot": { "room": { "working_directory": "<canonical>" } }
}
```

该事件只经当前 WebSocket sender 返回，不进入 collaboration broadcast。原有 `room_snapshot` 广播继续服务其他连接和旧客户端。

前端目录 operation 只由相同 `room_id + command_id` 的 accepted 或带相同身份的 error 结算，不再比较用户输入与 canonical 路径。accepted 携带权威快照，因此即使广播延迟或丢失，发起连接也能立即应用规范化结果并关闭 modal。相对路径、符号链接和 Windows extended path 不需要浏览器自行解析。

## 单调快照水位

协作 schema 升级到 v8，`collaboration_rooms` 增加：

```sql
state_revision INTEGER NOT NULL DEFAULT 0
```

每次事务成功插入新的 `room_changed` outbox 事件时，同一事务将房间 `state_revision` 加一。重复 idempotency key 未插入 outbox 时不增加水位。v7 迁移为现有房间写入初始非零 revision。

`CollaborationRoomView` 对外返回 `state_revision`。仓储快照本来就在一个 SQLite Deferred transaction 内读取 room、member、event、Inbox 和 delivery，因此 revision 与快照内容属于同一一致读视图。

同房间前端以 `(state_revision, room.version, latest_event_seq)` 作为完整 snapshot 的单调水位：三项都不得低于当前权威值，并且至少一项必须增长；旧或完全重复 snapshot 不再整体替换状态。这样 Inbox/member-only 变化由 `state_revision` 排序，旧数据库导入或事件追加仍可由既有 room/event 水位推进。`MemberChanged` 可以直接新增成员，`InboxItemChanged` 只以更高 `version` 更新权威窗口内的现存项，Inbox 新增与删除由紧随的完整 snapshot 权威处理。事件 append 不推进 snapshot 权威水位。

## 目录可用性与无损持久化

仓储提供一个共享目录验证函数，供设置入口、启动目录和 runtime pre-provider 校验复用：

1. `metadata` 成功且目标为目录；
2. Unix 权限位至少包含一个 search/execute 位；
3. `read_dir` 能打开目录，验证当前进程的实际读取/遍历权限；
4. 需要持久化时，canonical `Path` 必须能通过 `to_str()` 无损表示，否则返回中文配置错误。

该检查不调用 `set_current_dir`，不创建探针文件，也不提升工具权限。运行时仍使用 Claim 已冻结路径，不重新解析为当前房间或 startup 目录。检查失败沿现有 pre-execution 结算路径处理，provider 调用次数必须为零。

## 兼容性

- `command_id` 使用 serde default；旧客户端仍可发送旧目录命令并继续依赖广播快照。
- 新 accepted 事件是附加事件，旧客户端可忽略未知类型。
- v7 数据库原位迁移到 v8，不改已有目录、事件、Inbox 或 TaskRun。
- `state_revision` 与 room 乐观锁 `version` 分离，Inbox 运行状态变化不会制造发帖或目录更新冲突。

## 最小关键测试

遵循用户要求，不扩展为穷举测试矩阵；每类风险只保留一个关键合同测试：

1. 真实相对路径更新返回带 command identity 的 accepted，snapshot 中是 canonical 路径；纯前端 reducer 只由匹配 accepted 结算目录 operation。
2. Inbox-only 转换保持 room version/event sequence 不变但增加 `state_revision`；纯前端拒绝较旧/重复 revision，并允许更高实体 version 的单项更新。
3. Unix 无 search/execute 权限的冻结目录在 provider 前失败，query 计数为零。
4. Unix 非 UTF-8 canonical 路径被明确拒绝，不写入数据库。

其余既有目录、恢复、WebSocket、Node 与 workspace 门禁作为回归运行，不新增重复用例。
