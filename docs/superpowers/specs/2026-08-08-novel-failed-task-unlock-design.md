# 小说失败任务安全解锁工具设计

## 背景

小说工作流分别在 Task Engine 和 Novel Domain 中持久化执行状态。当前 Writer 执行失败时，Task Engine 会把运行标记为 `failed`，但 Novel checkpoint 可能仍停留在 `drafting` 且保持 `terminal=0`、`archived=0`。后续新任务会把这个 checkpoint 视为项目的活动工作，返回 `Novel project <project_id> already has active work`。

这个活动锁用于防止同一项目并发写作时出现 Canon 版本冲突、重复产物、审核对象错配和重复发布，不能直接无条件删除或归档。

## 目标

在现有 `novel_task` 工具中新增显式的 `unlock_failed` 操作，用于安全终结已失败或已取消、但仍占用项目活动锁的遗留任务。操作必须可审计、可重复调用、不得触发 LLM，也不得删除历史记录。

上线后使用该操作处理当前遗留任务 `wupo-guize-ch1-body-001`，然后验证项目锁已经释放。验证过程不得自动启动新的小说写作或产生新的付费 LLM 调用。

## 非目标

- 不提供强制解锁仍在 `queued`、`running` 或 `needs_input` 状态任务的能力。
- 不自动解锁后重新启动工作流。
- 不删除 checkpoint、Task Engine 运行记录、事件或错误信息。
- 不把解锁做成 `start` 的隐式副作用。
- 不处理已有发布中间态或已保存产物的任务。

## 工具契约

新增调用形式：

```json
{
  "action": "unlock_failed",
  "task_id": "wupo-guize-ch1-body-001",
  "reason": "Writer 执行失败后遗留活动锁"
}
```

字段约束：

- `action`：固定为 `unlock_failed`。
- `task_id`：必填，必须符合现有任务标识符约束。
- `reason`：必填，去除首尾空白后不能为空，最多 256 个 Unicode 字符，且不得包含 Unicode 控制字符，防止日志和审计事件注入。
- 该分支不接受 `project_id` 代替 `task_id`，避免项目级模糊匹配误解锁错误任务。

工具说明必须明确告诉模型：

1. 当 `novel_task.start` 返回 `project already has active work` 时，先查询 `novel_task.status` 并检查最新工作流错误。
2. 只有旧任务已经在 Task Engine 中进入 `failed` 或 `cancelled`，同时 Novel checkpoint 仍是非终态时，才调用 `unlock_failed`。
3. 若任务仍在运行、等待输入、已有草稿/候选/用户决定、进入发布流程或已经完成，应报告拒绝原因，不能反复重试解锁。
4. 解锁不会重试 LLM；成功后，如用户仍希望继续写作，应另行显式调用一次 `start`。

## 架构和职责

### Novel Workflow

在 `NovelStartWorkflow` 暴露只读的运行状态查询能力。查询由 workflow 使用它已有的 `TaskRepository` 完成，并沿用现有 `novel-task-{task_id}` 映射解析 Task Engine 运行记录。应用层不直接打开 `runtime.db`，从而保持存储边界一致。

查询结果区分：

- `failed` / `cancelled`：允许继续进行领域安全检查。
- `queued` / `running` / `paused_budget` / `needs_input`：拒绝解锁。
- `completed`：拒绝解锁，因为执行完成与 Novel checkpoint 不一致需要人工诊断，不能当作失败锁处理。
- 运行记录不存在：拒绝解锁，避免把未知状态当成失败。

### Novel Domain

新增一个语义明确的失败解锁转换，而不是复用仅针对会话分叉的 `cancel_for_conversation_fork`。转换仅允许非终态 checkpoint，并拒绝以下任一条件：

- 已存在 draft 或 candidate；
- 已存在主审记录或用户决定；
- 已进入 `publication_pending`、`artifact_saved_memory_pending` 等发布阶段；
- 已存在 artifact、publication id 或 commit report。

通过检查后，phase 转为 `cancelled`，checkpoint 自然成为 `terminal=1`，不再占用项目活动锁。

### Novel Application

新增异步 `unlock_failed_task(task_id, reason)` 用例：

1. 取得该任务的应用级互斥锁。
2. 加载 checkpoint；若 phase 已是 `cancelled`，返回 `already_unlocked=true` 的幂等成功，不再写事件。
3. 查询 Task Engine 运行状态并验证仅为 `failed` 或 `cancelled`。
4. 执行领域安全转换。
5. 使用现有事务化持久化路径保存 checkpoint，并以 `System` actor 写入 `manual_unlock` 生命周期事件；事件 details 保存经验证的 reason、原 phase 和 Task Engine 状态。
6. 返回包含 `task_id`、`project_id`、原 phase、新 phase、Task Engine 状态和审计原因的结构化结果。

持久化 checkpoint 和事件必须在现有数据库事务边界内完成，避免只释放锁却缺失审计记录。

### CLI 工具执行器

解析 `unlock_failed` 输入并调用应用服务。成功结果按 JSON 返回。所有拒绝都作为工具错误直接返回，错误信息明确说明当前 Task Engine 状态或领域保护条件，不触发 LLM 重试循环或跨 provider 回退。

## 并发和幂等性

- 解锁与 `resume`、`review`、`decide`、`publish` 使用同一 task lock，防止同一进程内交错修改。
- 在持久化前重新基于锁内加载的 checkpoint 做检查。
- 第一次成功后 checkpoint 为终态，因此同一 `task_id` 的重复 `unlock_failed` 返回原成功状态，不产生第二条状态转换。
- 项目中新任务仍由现有 project preflight 锁保护；解锁操作本身不负责启动新任务。

## 错误处理

典型错误应直接、可操作：

- `任务仍在 running，拒绝解锁；请等待任务结束或先执行受控取消。`
- `Task Engine 中不存在对应运行记录，拒绝把未知任务当作失败任务。`
- `任务已有草稿或候选版本，拒绝解锁；请通过 review/decide/publish 处理。`
- `任务处于发布中间态，拒绝解锁；请先恢复或诊断发布流程。`

错误文本不得包含 provider 原始响应体、密钥、Authorization 头或未限长的用户输入。

## 测试策略

按测试驱动方式覆盖：

1. 工具 schema 精确包含 `unlock_failed` 分支、必填字段和详细使用说明。
2. 输入验证拒绝缺失/空白/超长/含控制字符的 `reason` 以及多余字段。
3. Task Engine 为 `failed` 或 `cancelled` 且 checkpoint 无产物时，成功转为 `cancelled` 并释放活动锁。
4. Task Engine 为 `queued`、`running`、`paused_budget`、`needs_input`、`completed` 或不存在时，均拒绝且 checkpoint 不变。
5. 存在 draft、candidate、审核/决定、artifact 或发布中间态时拒绝。
6. 重复解锁幂等，不重复破坏状态。
7. 解锁不会调用 Writer/LLM。
8. 解锁后 `status` 不再返回活动任务，后续 `start` 的 preflight 不再因旧 checkpoint 报项目繁忙；测试中使用假 Writer，生产验证不调用 `start`。

## 部署与现有锁处理

完成定向测试、格式检查和可执行文件构建后，重启智脑使新工具生效。随后调用：

```json
{
  "action": "unlock_failed",
  "task_id": "wupo-guize-ch1-body-001",
  "reason": "Writer 旧版输出缺少 content，Task Engine 已失败但 Novel checkpoint 仍停留在 drafting"
}
```

最后只读取状态和数据库审计事件，确认：Task Engine 历史仍保留、Novel checkpoint 已终结、`manual_unlock` 事件存在、项目不再有活动任务，并确认没有新增 LLM 调用。

## 验收标准

- 模型能从工具说明识别活动锁故障并选择 `unlock_failed`，而不是持续重试 LLM。
- 工具无法解锁活跃、未知、已完成、有内容或发布中的任务。
- 成功解锁有完整审计记录，历史数据不被删除。
- 当前 `wupo-guize-ch1-body-001` 遗留锁被安全释放。
- 解锁和验证全过程不产生新的付费 LLM 调用。
