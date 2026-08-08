---
name: novel-writing-workflow
description: 统筹中文长篇小说的大纲、卷纲、章纲、正文、续写、审稿、润色、人物设定、剧情桥段和复盘，通过 Novel 领域应用完成可恢复任务、独立复审、用户确认、受控发布以及 Canon/知识投影。遇到小说创作、修改或审校任务时，在创建、修改、审校或发布作品前使用。
---

# Novel 写作工作流

项目文件和已提交 Canon 是作品事实的权威来源。`novel_project` 管理项目及 Canon 读模型，`novel_task` 管理可恢复的创作、复审、用户决策和发布。应用服务拥有内部状态转换、TaskRun、checkpoint、publication journal 和知识投影；不要在主脑中模拟这些状态。

## 应用入口

- `novel_project(action=create|list|recall|consistency|resolve_conflict)`：创建或查找项目、按任务类型召回 Canon、检查一致性、记录冲突处理结论。
- `novel_task(action=start|resume|review|decide|publish|status)`：启动或继续 durable task、提交主脑复审、记录用户决定、发布已封存候选稿、查询持久化状态。
- 始终使用稳定的 `project_id` 和 `task_id`。同一任务的澄清与修订沿用原 `task_id`。
- 严禁调用 `Agent(subagent_type='Novel')`，也不得直接写正式产物、提交 Canon 或操作 Memory/Graph 数据库。

## 主脑职责

### 建立项目和任务

1. 先用 `novel_project(action=list)` 查找项目；确认不存在时才用 `action=create` 创建。
2. 用 `action=recall` 获取当前 Canon revision，并用 `action=consistency` 检查未解决冲突、重叠事实和逾期伏笔。
3. 把任务归类为 `outline`、`volume_outline`、`chapter_plan`、`body`、`continuation`、`review`、`polish` 或 `retrospective`。
4. 明确目标章节、输出路径、视角、平台、文风、必须发生、禁止改变和可检查的验收标准。

### 准备受控上下文

1. 由主脑定位并核对待续写正文、上一章、章纲、卷纲、角色卡、世界观和风格样例。
2. 计算每项材料的 SHA-256，构造包含 `role`、受工作区约束路径、`sha256` 和可选描述的 `ContextRef`。
3. 调用 `novel_task(action=start)`，提交稳定 ID、任务合同、当前 revision、输出路径、ContextRef、约束和验收标准。默认使用 `publication_policy=require_user_acceptance`。
4. 仅当应用明确返回 `needs_clarification` 时，才向用户询问并调用 `novel_task(action=resume)`；沿用同一 `task_id`，且 `input` 必须是用户给出的非空澄清内容。其他错误不得用 `resume` 猜测恢复。
5. 若应用报告 ContextRef hash 已变化，重新读取该资源，使用错误中的 `actual` hash 更新调用，保持同一 `task_id` 并只重试一次；不得重复提交旧 hash。

### 独立复审

`draft_ready` 只是候选稿。主脑必须独立检查用户要求、章纲和前文承接、Canon、人物状态、时间线、剧情线、伏笔、视角、文风、重复、节奏和适用时的章末钩子。

通过 `novel_task(action=review)` 提交完整 typed review。Pass 时八项 checks 全部为 `pass`、issues 为空，并提供用户要求、章纲或前文、Canon 和 Artifact 的实际 evidence refs。Revise 时提供具体 issues；应用会在同一 task 中生成下一版。

### 用户决定和发布

主脑复审通过后，默认向用户展示候选正文并明确尚未发布。

- 用户接受：`novel_task(action=decide, decision=accept)`。
- 用户要求修改：使用同一 action，提交 `decision=revise` 和具体 feedback；随后重新复审新版本。
- 用户拒绝：提交 `decision=reject`；保留审计记录但不修改作品或 Canon。

不要替用户推断接受。只有应用确认可发布后才调用 `novel_task(action=publish)`，且只传 `task_id` 与已确认的 `draft_version`。不得再次传正文或用通用 `write_file` 保存正式小说产物。发布成功后向用户报告保存路径、内容 hash、draft version 和新 Canon revision。

## Writer 输出合同

Writer 只读取服务端冻结并验证过的任务合同、Canon recall 和授权上下文。关键信息不足时返回 `needs_clarification`，不直接向用户提问。

`draft_ready` 必须包含正文、六项自检、与当前 project/revision/source_ref 匹配的 `NovelMemoryDelta` 和证据引用。自检覆盖大纲对齐、Canon、人物、时间线、剧情与伏笔、风格与重复；未全部通过时不得声称候选稿就绪。

`NovelMemoryDelta` 只记录本轮新增的稳定设定、状态、事件、剧情线、伏笔、反馈和有效写作经验。没有新增时使用空数组，草稿和备选创意不得标为 Confirmed Canon。
