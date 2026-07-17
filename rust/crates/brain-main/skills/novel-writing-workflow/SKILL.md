---
name: novel-writing-workflow
description: 统筹中文长篇小说的大纲、卷纲、章纲、正文、续写、审稿、润色、人物设定、剧情桥段和复盘，协调主脑准备任务环境、常驻小说脑创作与自检、主脑独立复审、用户确认、MemoryBrain 生命周期记录和受控发布。遇到任何小说创作、修改或审校任务时，在调用常驻小说脑或发布作品前使用。
---

# 常驻小说脑写作工作流

项目文件是作品正文的权威来源，MemoryBrain 管理任务事件、checkpoint、Confirmed Canon、发布日志和图谱投影。常驻 NovelBrain 管理项目级短期工作态、创作、自检和连续修订。User 只与 MainBrain 对话。

## 共同门禁

- 始终使用稳定的 `project_id` 和 `task_id`；同一项目的修订沿用原 task，不创建一次性 Novel Agent。
- 严禁调用 `Agent(subagent_type='Novel')`。Explore、Plan、Verification 和 general-purpose 仍是临时 Agent。
- 小说脑不接收 memory root 或 graph path。所有长期记忆读写都经过 MemoryBrain，所有材料读取和作品发布都经过受限 ResourcePort。
- 把明确前文、章纲、角色卡、设定和用户要求置于模型推测之前；关键条件不足时让小说脑返回澄清请求，由主脑询问用户后调用 `novel_resume_task`。
- 草稿、自检、主脑 review、用户反馈属于生命周期记录，不是 Confirmed Canon。只有成功的 `novel_publish` 事务可以提交新的 Canon。
- 默认发布顺序不可跳过：小说脑自检 pass -> 主脑独立复审 pass -> 用户 accept -> 原子保存正文 -> MemoryBrain 完成 Canon commit。
- 只有用户在最初任务中明确要求生成后自动保存，才能设置 `publication_policy=auto_after_main_review`。

## 主脑分支

### 1. 建立项目与任务

1. 用 `novel_list_projects` 查找现有项目；确认不存在时再用 `novel_create_project` 创建。
2. 将任务归类为 `outline`、`volume_outline`、`chapter_plan`、`body`、`continuation`、`review`、`polish` 或 `retrospective`。
3. 明确目标章节、计划输出路径、视角、平台、文风、必须发生、禁止改变和可检查的验收标准。
4. 为本次完整生命周期生成稳定 `task_id`。用户修改、小说脑澄清和主脑退修都沿用这个 ID。

### 2. 准备任务环境

1. 由主脑查找相关文件，不把目录探索交给小说脑。
2. 优先定位待续写正文、上一章和必要前文、目标章纲、卷纲/总纲、角色卡、世界观设定、风格样例或待审稿件。
3. 读取关键材料确认它们确实匹配当前项目，并用当前环境可用的 shell 工具计算每个文件的 SHA-256。
4. 为每项材料构造 `ContextRef`：`role`、受工作区约束的路径、`sha256` 和可选描述。不要只传一句摘要代替正文或章纲。
5. 用 `novel_recall_project` 获取当前 Canon revision，用 `novel_check_consistency` 检查未解决冲突、重叠事实和逾期伏笔。

### 3. 启动或恢复常驻任务

调用 `novel_start_task`，完整提交：

- `task_id`、`project_id`、`task_type` 和清晰的 `task_brief`；
- `target_chapter`、当前 `expected_revision` 和最终 `output_path`；
- 带 hash 的 `context_refs`；
- `must_happen`、`must_not_change` 和 `acceptance_criteria`；
- 默认 `publication_policy=require_user_acceptance`。

若返回 `needs_clarification`，把问题整理后询问 User。用户下一轮回答时调用 `novel_resume_task(task_id, input)`；不要新建 task 或重新调用 Agent。

### 4. 独立复审

`draft_ready` 只是候选稿。主脑必须独立检查：

- 用户要求与任务合同；
- 章纲、前文和承接关系；
- Canon、人物状态、时间线、剧情线和伏笔；
- 视角、文风、重复、AI 腔、节奏及适用时的章末钩子。

通过 `novel_review_draft` 提交完整 `MainReviewRecord`。Pass 时八项 checks 必须全部为 `pass`、issues 为空，并提供用户要求、章纲/前文、Canon 等实际 evidence refs。Revise 时必须给出具体 issues；常驻小说脑会在同一 task/session 生成下一版，再重新复审。主脑不得悄悄大段改写后直接判定通过。

### 5. 展示候选稿并记录用户决定

主脑 Pass 后，默认把候选正文完整展示给 User，明确说明这是待确认版本，不要声称已经保存。

- User 接受：调用 `novel_user_decision`，decision=`accept`。
- User 要求修改：调用同一工具，decision=`revise` 并提交具体 feedback；小说脑在原 task 中生成新版本，随后重新执行主脑复审和用户确认。
- User 拒绝：调用同一工具，decision=`reject`；生命周期保留，但不得修改作品文件或 Canon。

不要替用户推断 accept。用户沉默、换话题或只评价局部内容都不等于发布授权。

### 6. 受控发布

只有状态进入 `approved_for_publication` 后，调用 `novel_publish`，且只传 `task_id` 与已确认的 `draft_version`。

- 不再次传正文；服务端从常驻任务状态取出已审核的精确内容。
- 不用通用 `write_file` 保存正式小说产物。
- 不直接提交 NovelMemoryDelta；MemoryBrain 在 artifact receipt 校验通过后完成 Canon commit。
- revision 过期、文件 hash 变化或 Canon 冲突时停止发布，重新召回并复审，不静默覆盖。
- 发布成功后向 User 汇报保存路径、内容 hash、draft version 和新的 Canon revision。

`novel_status` 用于查看 resident/idle、当前项目、task phase、draft version 和 pending publication，不要靠猜测判断任务是否仍在内存中。

## 常驻小说脑分支

### 1. 使用服务端环境

1. 读取主脑任务合同、MemoryBrain 返回的 Canon recall 和 ConsistencyReport。
2. 阅读服务端已验证 hash 的 `authorized_context` 全文；不得请求目录遍历、任意文件、memory root 或 graph path。
3. 若关键要求、承接正文或设定存在无法安全补全的缺口，返回 `needs_clarification`，不直接向 User 提问。

### 2. 生成与自检

- 大纲/卷纲：阶段目标、因果链、主线转折、人物推进和伏笔安排。
- 章纲：场景节拍、冲突升级、信息增量、承接点和章末钩子。
- 正文/续写：严格承接章纲与前文，保持人物动机、状态、时间线和叙事视角一致。
- 审稿：定位问题、给出证据和可执行修订方案，不把未经确认的改写当成 Canon。
- 润色：优化语言、动作、对话、感官细节与段落节奏，不改变剧情事实。
- 复盘：沉淀可复用方法、失败模式和用户反馈，不虚构作品事实。

完成后逐项检查 `outline_alignment`、`canon_consistency`、`character_consistency`、`timeline_consistency`、`plot_and_foreshadowing`、`style_and_repetition`。先修正问题；只有六项全部 pass、issues 为空时才能返回 `draft_ready`。

### 3. Typed JSON 交付

只返回服务端要求的单个 JSON 对象。`draft_ready` 必须包含正文、六项自检、与当前 project/revision/source_ref 匹配的 `NovelMemoryDelta` 和证据引用。不得声称已保存或已提交 Canon。

`NovelMemoryDelta` 只记录本轮新增的稳定设定、状态、事件、剧情线、伏笔、反馈和有效写作经验。没有新增时保留空数组，不得编造事实。草稿和备选创意不能标为 Confirmed Canon。
