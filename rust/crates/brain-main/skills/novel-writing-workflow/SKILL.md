---
name: novel-writing-workflow
description: 统筹中文长篇小说的大纲、卷纲、章纲、正文、续写、审稿、润色、人物设定、剧情桥段和复盘，协调主脑资料定位、项目 Canon 召回、Novel 子代理委派、双层审稿、文件保存及记忆提交。遇到任何小说创作、修改或审校任务时，在调用 Novel Agent 或写入小说文件前使用。
---

# 小说写作工作流

把项目快照视为权威 Canon，把图谱视为按需索引。根据当前身份执行对应分支，不要让小说脑重复主脑已经完成的目录探索。

## 共同门禁

- 始终使用稳定的 `project_id`，不得跨项目读取或提交事实。
- 把明确前文、章纲、设定和用户要求置于模型推测之前；信息不足时只做最小、可逆且明确标注的假设。
- 默认禁止 Web 调研。仅当用户任务确实需要外部资料时设置 `allow_web_research=true`，只提炼机制与读者预期，不照搬表达、角色或情节。
- 把通用评估脑与小说审稿分开；无论通用评估脑是否启用，都要完成小说脑自检和主脑独立复审。

## 主脑分支

### 1. 建立项目与任务

1. 用 `novel_list_projects` 查找现有项目；确认不存在时再用 `novel_create_project` 创建。
2. 将任务归类为 `outline`、`volume_outline`、`chapter_plan`、`body`、`continuation`、`review`、`polish` 或 `retrospective`。
3. 明确目标章节、计划输出路径、字数/结构、视角、平台、文风、必须发生、禁止改变和验收标准。

### 2. 定位完整材料

1. 由主脑查找并读取相关文件，不把目录探索交给小说脑。
2. 优先定位目标章纲、上一章及必要的更早承接章节、卷纲/总纲、人物或世界观设定、风格样例和待审/待润色稿。
3. 记录每个文件的精确路径与角色。不要用一句话摘要替代可直接读取的正文路径。
4. 用 `novel_recall_project` 按任务类型召回结构化 Canon，再用 `novel_check_consistency` 获取冲突、重叠事实和逾期伏笔。

### 3. 构造委派合同

同步调用 `Agent(subagent_type='Novel')`，并完整填写 `novel_context`：

- `project_id`：当前小说项目。
- `task_type`：本轮任务类型。
- `target_chapter`：适用时填写目标章节。
- `canon_revision`：本次召回得到的当前 revision。
- `output_path`：主脑计划在复审通过后保存的路径。
- `context_files`：精确文件清单，每项包含 `role`、`path` 和可选 `description`。
- `must_happen`：本轮必须发生或覆盖的事项。
- `must_not_change`：不得改写的剧情事实、人设、时间线和视角约束。
- `acceptance_criteria`：至少一条可检查的验收条件。
- `allow_web_research`：默认 `false`。

在 `prompt` 中描述具体产物和本轮重点，不重复粘贴已经通过路径和 Canon 工具可获得的大段材料。

### 4. 独立复审

1. 检查 Agent 返回的 `requiresMainReview=true`，以及 `novelSelfReview.present=true`、`valid=true`、`verdict=pass`。
2. 从 `[NOVEL_CONTENT]` 提取候选产物，独立对照用户要求、章纲、相关前文、Canon、一致性报告、人物状态、时间线、剧情线、伏笔和委派合同。
3. 检查剧情承接与遗漏、设定和人设冲突、因果链、时间线、视角、重复、AI 腔、文风、节奏及适用时的章末钩子。
4. 发现问题时形成具体修订清单，沿用相同 `novel_context` 并更新 revision 后重新委派 Novel；不要由主脑悄悄大段改写后直接判定通过。

### 5. 保存与提交

1. 仅在小说脑自检和主脑复审均通过后保存 `[NOVEL_CONTENT]`。
2. 确认文件保存成功后，再从 `[NOVEL_MEMORY_DELTA]` 提取 JSON 并调用 `novel_commit_delta`。
3. 遇到 revision 过期或 Canon 冲突时停止提交、重新召回并复审；用 `novel_resolve_conflict` 记录审查结论，事实修改仍通过新的 Delta 提交。

## 小说脑分支

### 1. 读取与召回

1. 先解析主脑给出的 `novel_context`，再按需读取 `context_files`；不得遍历目录或读取清单外文件。
2. 调用 `novel_recall_project` 获取当前项目完整结构化 Canon，并在写作前调用 `novel_check_consistency`。
3. 仅在具体疑点上查询 Novel 图谱；只查询委派的项目。仅在 `allow_web_research=true` 时使用 Web 工具。

### 2. 生成产物

- 大纲/卷纲：给出阶段目标、因果链、主线转折、人物推进和伏笔安排。
- 章纲：给出场景节拍、冲突升级、信息增量、承接点和章末钩子。
- 正文/续写：严格承接章纲与前文，保持人物动机、状态、时间线和叙事视角一致。
- 审稿：定位问题、给出证据和可执行修订方案，不把未经确认的改写当成 Canon。
- 润色：优化语言、动作、对话、感官细节与段落节奏，不改变剧情事实。
- 复盘：沉淀可复用方法、失败模式和用户反馈，不虚构作品事实。

### 3. 自检并先修正

重新对照所有输入，逐项检查：

- `outline_alignment`：章纲、任务目标和必须发生事项。
- `canon_consistency`：世界观、事件和既有事实。
- `character_consistency`：人设、动机、关系和当前状态。
- `timeline_consistency`：时间顺序、地点移动和伤势/物品状态。
- `plot_and_foreshadowing`：剧情线、遗漏情节、伏笔铺设与回收。
- `style_and_repetition`：视角、文风、节奏、重复和 AI 腔。

先修正发现的问题，再输出自检报告。任一检查失败时使用 `needs_revision`，不得用空检查表或空泛结论冒充通过。

### 4. 按协议交付

严格输出三个区块，不在 `[NOVEL_CONTENT]` 中混入解释、审稿元数据或 JSON：

```text
[NOVEL_CONTENT]
可直接保存的最终产物
[/NOVEL_CONTENT]

[NOVEL_SELF_REVIEW]
{"verdict":"pass|needs_revision","issues":[],"checks":{"outline_alignment":"pass|fail","canon_consistency":"pass|fail","character_consistency":"pass|fail","timeline_consistency":"pass|fail","plot_and_foreshadowing":"pass|fail","style_and_repetition":"pass|fail"},"unverified_assumptions":[]}
[/NOVEL_SELF_REVIEW]

[NOVEL_MEMORY_DELTA]
单个 NovelMemoryDelta JSON 对象
[/NOVEL_MEMORY_DELTA]
```

使用以下最小 Delta 结构：

```json
{"project_id":"...","branch_id":"main","expected_revision":0,"task_type":"outline|volume_outline|chapter_plan|body|continuation|review|polish|retrospective","source_ref":"计划保存路径","progress":{"current_volume":null,"current_chapter":null},"proposed_facts":[],"state_changes":[],"plot_updates":[],"foreshadowing_updates":[],"feedback":[],"experience_candidates":[]}
```

只记录本轮新增的稳定设定、状态、事件、剧情线、伏笔、反馈和有效写作经验。沿用召回包的 `project_id`、`branch_id` 和 revision；纯假设或备选创意标为 draft。没有新增时保留空数组，不要编造 Delta。
