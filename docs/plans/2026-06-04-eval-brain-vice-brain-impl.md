# 评估脑副脑化改造 实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 将评估脑从"质量审核员"改造成主脑的副脑，负责结论验证和用户要求合规检查，通过 Skill 驱动不同任务类型的审查。

**Architecture:** 保留现有 eval_tool_loop / SkillRegistry / checker 基础设施。改造核心是：重写系统 prompt（角色定义+技能路由表+用户画像注入）+ 扩展 evaluate() 签名 + 新建审查 Skills + 编排器传入画像数据。

**Tech Stack:** Rust, brain-eval crate, brain-memory crate (PyramidMemoryBrain), ai-brain-cli (Orchestrator)

**Design Doc:** `docs/plans/2026-06-04-eval-brain-vice-brain-design.md`

---

### Task 1: 记忆脑暴露画像和踩坑数据

**Files:**
- Modify: `rust/crates/brain-memory/src/pyramid_memory_brain.rs` (在 `load_eval_requirements` 附近)

**Step 1: 在 PyramidMemoryBrain 上新增两个公开方法**

```rust
/// 获取用户画像摘要文本（用于注入评估脑 prompt）
pub fn load_profile_summary(&self) -> Result<String> {
    let store = ProfileStore::new(self.storage.clone());
    store.summary()
}

/// 获取活跃踩坑记录（用于评估脑用户要求合规检查）
pub fn load_active_pitfalls(&self) -> Vec<brain_core::types::PitfallRecord> {
    let store = PitfallStore::new(self.storage.clone());
    store.load_active().unwrap_or_default()
}
```

需要在文件顶部添加 `use crate::pitfall::PitfallStore;` 和 `use crate::profile_eval::ProfileStore;`（检查是否已导入）。

**Step 2: 验证编译**

Run: `cd rust && cargo check -p brain-memory`
Expected: 编译通过，无错误

**Step 3: Commit**

```bash
git add rust/crates/brain-memory/src/pyramid_memory_brain.rs
git commit -m "feat(memory): 暴露 load_profile_summary 和 load_active_pitfalls 方法"
```

---

### Task 2: 新建 troubleshooting-verification Skill

**Files:**
- Create: `rust/crates/brain-eval/skills/troubleshooting-verification/SKILL.md`

**Step 1: 创建 Skill 文件**

```markdown
---
name: troubleshooting-verification
description: "排查问题结论验证。当主脑进行故障排查、问题分析、根因定位时适用：验证结论是否有日志/数据佐证，搜集证据证实或证伪。"
---

# 排查问题结论验证

## Overview
主脑进行故障排查后给出的结论必须有数据佐证。不能只凭推理得出结论，必须有日志、链路追踪、代码证据、配置数据等支撑。

## The Iron Law
```
NO TROUBLESHOOTING CONCLUSION WITHOUT DATA EVIDENCE

主脑说"根因是 X" = 必须有日志/代码/数据证明 X 存在
主脑说"问题是 Y 导致的" = 必须有证据证明 Y 确实发生了
主脑说"解决方案是 Z" = 必须验证 Z 是否可执行且能解决问题
推测性结论 = 必须标注为推测，不能声称已确认
```

## The Gate Function
```
BEFORE 接受排查结论：

1. IDENTIFY: 主脑给出了什么排查结论？
   - 根因定位："问题是 X 导致的"
   - 状态判断："服务 Y 正常/异常"
   - 影响范围："影响了 Z 个用户"
   - 解决方案："需要执行 W 操作"

2. CHECK EVIDENCE: 主脑基于什么数据得出结论？
   - 有没有查看相关日志？
   - 有没有查看相关代码/配置？
   - 有没有查看链路追踪数据？
   - 有没有执行验证命令？

   IF 主脑只做了推理没有查看数据 → 报告"结论缺乏数据佐证"

3. VERIFY: 用工具搜集证据验证结论
   - grep_search → 搜索日志中是否有主脑声称的错误
   - read_file → 查看主脑引用的代码/配置是否真实存在
   - bash → 执行验证命令确认主脑的判断

   IF 搜集到的证据与结论矛盾 → 报告 [Critical] "结论被证伪"
   IF 搜集到的证据支持结论 → 标记为已验证

4. CHECK ALTERNATIVES: 是否有其他可能？
   - 主脑是否考虑了其他可能的原因？
   - 是否存在更简单的解释？
   - 排查过程是否完整？

5. ONLY THEN: 给出验证结果
```

## Common Failures
| 主脑声明 | 需要验证 | 证伪情况 |
|---------|---------|---------|
| "根因是数据库超时" | grep 日志中是否有 timeout 错误 | 日志无 timeout 记录 |
| "这个 bug 在 XX 行" | read_file 查看该行代码 | 代码不存在所述内容 |
| "配置项 Y 的值是 Z" | read_file 配置文件 | 实际值不是 Z |
| "修改后问题解决" | 执行验证命令 | 问题仍存在 |
| "可能是缓存问题" | 检查缓存状态 | 缓存状态正常 |

## Red Flags - STOP
- 主脑给出根因但没有引用任何日志/代码/数据
- 主脑说"确认是 X"但只有推理没有验证
- 主脑排查过程中没有调用任何工具（纯推理排查）
- 主脑的结论与已知证据矛盾
- 主脑跳过了明显需要检查的环节

## Rationalization Prevention
| 借口 | Reality |
|------|---------|
| "主脑推理很合理" | 合理 ≠ 正确，用数据验证 |
| "主脑引用了日志" | read_file 确认日志内容 |
| "主脑说很确定" | 确定 ≠ 正确，用工具验证 |
| "主脑给了详细分析" | 详细 ≠ 有证据支撑 |
| "主脑是资深工程师" | 任何人排查都可能出错 |

## Key Patterns
```
✅ [主脑说"根因是数据库超时"] → [grep "timeout" 日志] [See: 3 matches] → [read_file 代码] [See: 未设超时] → "结论有证据"
❌ [主脑说"根因是数据库超时"] → "分析很有道理" / 不验证

✅ [grep 无匹配] → "评估结果-存在问题。具体问题：[Critical] 主脑声称的根因'数据库超时'无日志佐证，日志中未发现 timeout 错误"
❌ [grep 无匹配] → "评估结果-正常"

✅ [主脑说"可能是缓存"] → [grep 日志] [See: 有缓存miss] → "推测有初步证据，但未确认"
❌ [主脑说"可能是缓存"] → [grep 日志] [See: 无缓存问题] → "评估结果-存在问题。[Critical] 推测与数据不符"
```

## When To Apply
- 主脑进行了故障排查、问题分析
- 主脑操作轨迹中有 grep/read_file/bash 查日志、查配置、查代码
- 主脑给出了问题原因、根因定位、解决方案
- 用户问题是"为什么..."/"是什么原因"/"排查一下"

## When NOT To Apply
- 主脑只是执行了代码修改（用 code-verification）
- 主脑输出是写作/创作内容（用 writing-verification）
- 纯闲聊问答（用 conclusion-verification）

## The Bottom Line
排查结论必须有数据佐证。纯推理不验证 = 猜测 = 必须报告。用工具搜集证据证实或证伪，发现矛盾必须标记为 Critical。
```

**Step 2: 验证 Skill 被加载**

Run: `cd rust && cargo test -p brain-eval -- --test-threads=1`
Expected: 所有测试通过（新 Skill 会被 load_from_dir 自动加载）

**Step 3: Commit**

```bash
git add rust/crates/brain-eval/skills/troubleshooting-verification/
git commit -m "feat(eval): 新增 troubleshooting-verification 审查技能"
```

---

### Task 3: 新建 writing-verification Skill

**Files:**
- Create: `rust/crates/brain-eval/skills/writing-verification/SKILL.md`

**Step 1: 创建 Skill 文件**

```markdown
---
name: writing-verification
description: "写作内容评估。当主脑进行写作、创作、文案生成时适用：检查是否违反用户写作规则、引用数据是否有来源、逻辑是否自洽。"
---

# 写作内容评估

## Overview
主脑输出的写作/创作内容需要检查：是否违反用户定义的写作规则、引用数据是否有来源、内容逻辑是否自洽。不评估文学质量（主观），只检查客观正确性。

## The Iron Law
```
FACTUAL CLAIMS IN WRITING MUST HAVE SOURCES
USER WRITING RULES ARE MANDATORY

引用了数据/事实 = 必须有来源
用户定义了写作规则 = 必须遵守
前后矛盾 = 逻辑错误 = 必须报告
风格/质量 = 主观 = 不评估
```

## The Gate Function
```
BEFORE 接受写作输出：

1. IDENTIFY: 主脑输出了什么类型的写作？
   - 小说/故事
   - 技术文档
   - 文案/邮件
   - 分析报告

2. CHECK USER RULES: 用户是否定义了写作规则？
   - 查看「用户要求」中的禁忌/偏好
   - 如果用户要求了特定风格/格式/规则 → 检查是否遵守
   - 禁忌词/禁忌主题 → Critical 级别

   IF 违反用户写作禁忌 → 报告 [Critical]

3. VERIFY FACTS: 写作中引用的数据/事实
   - 引用了数据 → 有没有来源？
   - 引用了代码 → 有没有验证代码是否存在？
   - 引用了历史/科学事实 → 是否准确？

   IF 引用的数据无来源 → 报告 [Warning] "引用数据缺乏来源"
   IF 引用的内容不准确 → 报告 [Critical] "事实错误"

4. CHECK CONSISTENCY: 内容逻辑自洽性
   - 前后描述是否矛盾？
   - 时间线是否合理？
   - 引用的前后文是否一致？

   IF 发现矛盾 → 报告 [Warning] "内容前后矛盾"

5. ONLY THEN: 给出评估结果
```

## Common Failures
| 问题类型 | 检查方法 | 报告级别 |
|---------|---------|---------|
| 违反用户禁忌词/主题 | 对照用户禁忌列表 | Critical |
| 引用数据无来源 | 检查是否有出处标注 | Warning |
| 引用代码不存在 | grep/read_file 验证 | Critical |
| 前后矛盾 | 对比上下文 | Warning |
| 违反用户格式要求 | 对照用户偏好 | Warning |

## Red Flags - STOP
- 主脑写作中包含了用户明确禁止的内容
- 主脑引用了数据但没有给出任何来源
- 主脑引用的代码/文件路径不存在
- 主脑描述的事实与已知信息矛盾
- 主脑输出的格式违反了用户明确要求的格式

## Rationalization Prevention
| 借口 | Reality |
|------|---------|
| "这只是创意写作" | 创意写作中引用的事实仍需准确 |
| "用户不会注意到" | 用户明确禁止的内容必须检查 |
| "引用的数据大致准确" | 大致 ≠ 准确，必须可验证 |
| "前后矛盾不明显" | 任何矛盾都应报告 |

## Key Patterns
```
✅ [主脑引用"2024年统计数据"] → [检查有无来源标注] [无来源] → "评估结果-存在问题。[Warning] 引用数据'2024年统计数据'缺乏来源"
❌ [主脑引用"2024年统计数据"] → "数据看起来合理" / 不验证

✅ [用户禁止"穿越元素"] → [主脑输出含"穿越"] → "评估结果-存在问题。[Critical] 违反用户写作禁忌：包含穿越元素"
❌ [用户禁止"穿越元素"] → [主脑输出含"穿越"] → "写得不错"

✅ [主脑第2章说"主角25岁"] → [第5章说"主角20年经验"] → [矛盾] → "评估结果-存在问题。[Warning] 25岁角色不可能有20年经验，前后矛盾"
❌ [发现矛盾] → "读者可能不会注意到"
```

## When To Apply
- 主脑输出了长文本内容（>500字）
- 主脑的操作轨迹中没有 edit_file/write_file（非代码任务）
- 用户任务是写作/创作/文案/文档
- 主脑输出包含事实性陈述

## When NOT To Apply
- 主脑进行了代码修改（用 code-verification）
- 主脑进行了排查分析（用 troubleshooting-verification）
- 简短问答（用 conclusion-verification）

## The Bottom Line
写作内容中的事实性声明必须有来源，用户定义的写作规则必须遵守，内容逻辑必须自洽。风格质量不评估，但客观错误必须报告。
```

**Step 2: 验证编译 + 测试**

Run: `cd rust && cargo test -p brain-eval -- --test-threads=1`
Expected: 通过

**Step 3: Commit**

```bash
git add rust/crates/brain-eval/skills/writing-verification/
git commit -m "feat(eval): 新增 writing-verification 审查技能"
```

---

### Task 4: 重写系统 Prompt（角色定义 + 技能路由表 + 用户画像注入）

**Files:**
- Modify: `rust/crates/brain-eval/src/prompts.rs`

**Step 1: 重写 `build_evaluation_system_prompt` 函数**

关键改动点：
1. 角色定义从"质量审核员"改为"主脑的副脑"
2. 评估流程从"三步法"改为"四步法"（识别任务→加载Skill→证据验证→用户合规）
3. 新增技能路由表
4. 新增用户画像注入段（接收 `profile_summary: Option<&str>` 和 `pitfalls: Option<&[PitfallRecord]>` 参数）
5. 保留环境信息、available_skills、输出格式

函数签名改为：
```rust
pub fn build_evaluation_system_prompt(
    eval_requirements: &[EvalRequirement],
    registry: &SkillRegistry,
    with_tools: bool,
    profile_summary: Option<&str>,           // 新增：画像摘要文本
    pitfall_descriptions: Option<&[String]>,  // 新增：踩坑描述列表
) -> String
```

角色定义段改为：
```
# 角色定义

你是主脑的副脑。你的唯一职责是验证主脑的结论是否正确、是否有数据支撑、是否违反用户要求。

你不做需求满足度检查——那是主脑自己的事。你只关注两件事：
1. 主脑的结论是否经得起验证（有数据/代码/日志佐证）
2. 主脑是否违反了用户在各会话中明确或隐含的要求
```

四步法改为：
```
## 评估流程（四步法）

### 第一步：识别任务类型
从主脑的操作轨迹判断主脑做了什么类型的任务。

### 第二步：加载对应审查技能
按技能路由表调用 Skill 工具，获取该任务类型的审查规则。

### 第三步：证据搜集验证
调用只读工具（read_file、grep_search、bash）搜集佐证数据。
验证主脑的每个事实性断言——能证实的标记为已验证，能证伪的标记为问题。
用户提出的问题，主脑的回答必须有事实性数据/案例/代码支撑。

### 第四步：用户要求合规检查
对照用户画像（禁忌/习惯/偏好）+ 踩坑记录 + 用户评估要求。
检查主脑输出是否违反了任何用户要求。
```

技能路由表新增：
```
## 技能路由表

根据主脑操作轨迹判断任务类型，加载对应 Skill：

| 任务类型 | 判断依据 | 加载的 Skill |
|---------|---------|-------------|
| 排查问题 | 主脑调用了 grep/read_file/bash 查日志、查链路、查配置 | troubleshooting-verification |
| 代码修改 | 主脑调用了 edit_file/write_file | code-verification |
| 写作/创作 | 主脑输出了长文本（>500字），无 edit_file/write_file | writing-verification |
| 通用问答 | 简短回答、事实性断言、其他类型 | conclusion-verification |

**用户提出的问题必须走深度验证：** 主脑回答中必须有事实性数据、案例、代码等支撑，不能只有推理。
```

用户画像注入段新增：
```rust
// 用户画像注入
if let Some(summary) = profile_summary {
    prompt.push_str("# 用户画像\n\n");
    prompt.push_str(summary);
    prompt.push_str("\n\n");
}

// 踩坑记录注入
if let Some(pitfalls) = pitfall_descriptions {
    if !pitfalls.is_empty() {
        prompt.push_str("# 踩坑记录（主脑不能重复犯的错误）\n\n");
        for (i, desc) in pitfalls.iter().enumerate() {
            let _ = writeln!(prompt, "{}. {}", i + 1, desc);
        }
        prompt.push('\n');
    }
}
```

严重程度判定新增：
```
## 严重程度判定

- Critical（必须通知用户）：事实性错误、结论被证伪、违反用户禁忌、重复踩坑
- Warning（自动修正）：非关键建议、风格问题、非最佳实践

输出格式中用 [Critical] 或 [Warning] 标记每个问题的严重程度。
```

输出格式调整为：
```
没有问题时：
评估结果-正常

有问题时：
评估结果-存在问题。具体问题：1.[Critical/Warning] 问题描述及修正建议 2.[Critical/Warning] 问题描述及修正建议
```

**Step 2: 更新 `build_evaluation_user_prompt`**

当前内容不变，但在末尾的提示语从：
```
请按照三步法评估：1)理解用户需求 → 2)对照主脑输出 → 3)二次校验。
```
改为：
```
请按照四步法评估：1)识别任务类型 → 2)加载对应Skill → 3)证据搜集验证 → 4)用户要求合规检查。
```

**Step 3: 更新所有测试用例**

现有测试需要适配新签名：
- `build_evaluation_system_prompt(&[], &SkillRegistry::new(), false)` → `build_evaluation_system_prompt(&[], &SkillRegistry::new(), false, None, None)`
- 新增测试：`system_prompt_has_vice_brain_role` — 验证包含"副脑"
- 新增测试：`system_prompt_has_skill_routing` — 验证包含"技能路由表"
- 新增测试：`system_prompt_with_profile_summary` — 验证画像注入
- 新增测试：`system_prompt_with_pitfall_descriptions` — 验证踩坑注入
- 删除/修改不适用于新角色的断言（如 "质量审核员"、"宁漏勿报"等旧断言）

**Step 4: 验证编译 + 测试**

Run: `cd rust && cargo test -p brain-eval -- --test-threads=1`
Expected: 所有测试通过

**Step 5: Commit**

```bash
git add rust/crates/brain-eval/src/prompts.rs
git commit -m "feat(eval): 重写系统prompt - 副脑角色 + 技能路由表 + 用户画像注入"
```

---

### Task 5: 扩展 evaluate() 方法签名

**Files:**
- Modify: `rust/crates/brain-eval/src/eval_brain.rs`

**Step 1: 修改 `evaluate()` 方法签名**

```rust
pub async fn evaluate(
    &self,
    user_input: &str,
    ai_output: &str,
    turns: &[TurnRecord],
    eval_requirements: &[EvalRequirement],
    profile_summary: Option<&str>,           // 新增
    pitfall_descriptions: Option<&[String]>,  // 新增
) -> Result<EvalResult>
```

在 `evaluate()` 内部，将新参数传给 `build_evaluation_system_prompt`：
```rust
let system_prompt = prompts::build_evaluation_system_prompt(
    eval_requirements,
    &self.skill_registry,
    true,
    profile_summary,
    pitfall_descriptions,
);
```

**Step 2: 更新所有调用 `build_evaluation_system_prompt` 的地方**

在 `eval_brain.rs` 和 `prompts.rs` 的测试中搜索所有调用点，添加 `None, None` 参数。

**Step 3: 更新 `eval_brain.rs` 中的测试**

所有 `brain.evaluate(...)` 调用添加 `None, None`：
```rust
// 之前：
brain.evaluate("写代码", "fn add() {}", &[], &[]).await
// 之后：
brain.evaluate("写代码", "fn add() {}", &[], &[], None, None).await
```

**Step 4: 新增测试：evaluate 接收画像和踩坑数据**

```rust
#[tokio::test]
async fn evaluate_with_profile_and_pitfalls() {
    let llm = Arc::new(MockLlmProvider::new("评估结果-正常"));
    let brain = EvalBrain::new(llm);
    let result = brain
        .evaluate(
            "写代码",
            "fn add() {}",
            &[],
            &[],
            Some("用户偏好 Rust，禁忌使用 unwrap"),
            Some(&["使用 unwrap 导致 panic".into()]),
        )
        .await
        .unwrap();
    assert!(result.passed);
}
```

**Step 5: 验证编译 + 测试**

Run: `cd rust && cargo test -p brain-eval -- --test-threads=1`
Expected: 所有测试通过

**Step 6: Commit**

```bash
git add rust/crates/brain-eval/src/eval_brain.rs
git commit -m "feat(eval): evaluate() 方法签名扩展，支持画像和踩坑数据"
```

---

### Task 6: 编排器接入画像数据 + Severity 分层处理

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`

**Step 1: 在评估脑调用处加载画像和踩坑数据**

找到编排器中 `eb.evaluate(...)` 调用（约 line 822-855），将：
```rust
let eval_requirements = {
    let mem = this.memory_brain.lock().await;
    mem.load_eval_requirements()
};
```
改为：
```rust
let (eval_requirements, profile_summary, pitfall_descriptions) = {
    let mem = this.memory_brain.lock().await;
    let reqs = mem.load_eval_requirements();
    let profile = mem.load_profile_summary().ok();
    let pitfalls = mem.load_active_pitfalls();
    let pitfall_descs: Vec<String> = pitfalls.iter().map(|p| p.description.clone()).collect();
    (reqs, profile, pitfall_descs)
};
```

将 `eb.evaluate(...)` 调用改为：
```rust
eb.evaluate(
    &input_owned,
    &answer,
    &result.as_ref().unwrap().turns,
    &eval_requirements,
    profile_summary.as_deref(),
    Some(&pitfall_descriptions),
).await
```

**Step 2: Severity 分层处理**

在评估结果处理逻辑（约 line 858-920）中，修改 `!passed` 的处理：

```rust
if eval_result.passed {
    tracing::info!("v2 评估通过 (第{}次)", attempt + 1);
    break;
}

// 判断严重程度
let has_critical = eval_result.feedback.contains("[Critical]");

if has_critical {
    // Critical: 通知用户，不自动重试
    tracing::warn!(
        "v2 评估发现 Critical 问题(第{}次): {}",
        attempt + 1,
        truncate_chars(&eval_result.feedback, 300)
    );
    // 仍然注入反馈给主脑看，但不自动重试
    brain.push_evaluator_to_history(&eval_result.feedback);
    break;  // 让用户看到当前输出 + 评估反馈
}

// Warning: 自动注入历史，重试（保持现有逻辑）
tracing::warn!(
    "v2 评估发现 Warning 问题(第{}次): {}",
    attempt + 1,
    truncate_chars(&eval_result.feedback, 200)
);
brain.push_evaluator_to_history(&eval_result.feedback);
// ... 继续重试逻辑（不变）
```

**Step 3: 验证编译**

Run: `cd rust && cargo check -p ai-brain-cli`
Expected: 编译通过

**Step 4: Commit**

```bash
git add rust/crates/ai-brain-cli/src/orchestrator.rs
git commit -m "feat(orchestrator): 评估脑接入画像数据 + Critical/Warning 分层处理"
```

---

### Task 7: 全量编译 + 测试验证

**Files:** 无新改动

**Step 1: 全量编译**

Run: `cd rust && cargo check --workspace`
Expected: 无错误

**Step 2: 全量测试**

Run: `cd rust && cargo test --workspace --exclude brain-integration-tests`
Expected: 所有测试通过

**Step 3: 确认 Skill 加载**

Run: `cd rust && cargo test -p brain-eval -- --test-threads=1`
Expected: 通过，新 Skills 被正确加载

**Step 4: Final Commit**

```bash
git add -A
git commit -m "feat(eval): 评估脑副脑化改造完成 - 结论验证 + 用户要求合规 + Skill驱动"
```

---

## 改动汇总

| Task | 改动 | 文件数 |
|------|------|--------|
| 1 | 记忆脑暴露方法 | 1 |
| 2 | troubleshooting-verification Skill | 1 (新建) |
| 3 | writing-verification Skill | 1 (新建) |
| 4 | 系统 Prompt 重写 | 1 |
| 5 | evaluate() 签名扩展 | 1 |
| 6 | 编排器接入 + 分层处理 | 1 |
| 7 | 全量验证 | 0 |
| **Total** | | **6 文件** |
