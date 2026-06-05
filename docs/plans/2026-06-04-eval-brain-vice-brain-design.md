# 评估脑副脑化改造设计

> 日期：2026-06-04
> 状态：已批准

## 背景

当前评估脑（brain-eval）角色为"质量审核员"，使用三步法（理解需求 → 对照输出 → 二次校验）评估主脑输出。
问题：
1. 只做需求满足度检查，不做结论正确性验证（主脑的断言是否有数据/代码/日志佐证）
2. 用户要求合规性依赖 EvalRequirement（用户反馈），未利用记忆脑的用户画像/踩坑库全量数据
3. 不同任务类型（排查问题、代码修改、写作、问答）应该用不同的审查策略，但当前只有通用三步法

## 改造目标

评估脑从"质量审核员"变成**主脑的副脑**，核心职责：
1. **结论验证** — 主脑的事实性断言是否有数据/代码/日志佐证，能被证伪或证实
2. **用户要求合规** — 是否违反用户在各会话中定义的要求（画像 + 踩坑 + 评估要求）

## 改造原则

- **最小改造**：不改 eval_brain 核心架构（tool_loop、SkillRegistry、checker 保留）
- **Skill 驱动**：不同任务类型通过 Skill 文件定义审查规则，评估脑 LLM 自动路由
- **分层处理**：Warning 级自动重试，Critical 级通知用户

## 设计细节

### 1. 系统 Prompt 改造（prompts.rs）

#### 1.1 角色定义

从"质量审核员"改为"主脑的副脑"：

```
你是主脑的副脑。你的唯一职责是验证主脑的结论是否正确、是否有数据支撑、是否违反用户要求。
你不做需求满足度检查——那是主脑自己的事。你只关注两件事：
1. 主脑的结论是否经得起验证（有数据/代码/日志佐证）
2. 主脑是否违反了用户在各会话中明确或隐含的要求
```

#### 1.2 评估流程（四步法）

```
第一步：识别任务类型
  从主脑的操作轨迹判断主脑做了什么类型的任务。

第二步：加载对应审查技能
  按技能路由表调用 Skill 工具，获取该任务类型的审查规则。

第三步：证据搜集验证
  调用只读工具（read_file、grep_search、bash）搜集佐证数据。
  验证主脑的每个事实性断言——能证实的标记为已验证，能证伪的标记为问题。

第四步：用户要求合规检查
  对照用户画像（禁忌/习惯/偏好）+ 踩坑记录 + 用户评估要求。
  检查主脑输出是否违反了任何用户要求。
```

#### 1.3 技能路由表（嵌入 prompt）

| 任务类型 | 判断依据 | 应加载的 Skill |
|---------|---------|--------------|
| 排查问题 | 主脑调用了 grep/read_file/bash 查日志、查链路、查配置 | `troubleshooting-verification` |
| 代码修改 | 主脑调用了 edit_file/write_file | `code-verification` |
| 写作/创作 | 主脑输出了长文本（>500字），无工具调用或只有搜索类工具 | `writing-verification` |
| 通用问答 | 简短回答、事实性断言、其他类型 | `conclusion-verification` |

#### 1.4 用户画像注入段

在系统 prompt 中新增：

```
## 用户要求（来自记忆脑）

### 用户画像
- 禁忌：{taboos}（必须严格遵守，违反即 Critical）
- 习惯：{habits}（参考性约束）
- 显性偏好：{explicit_preferences}
- 隐性偏好：{implicit_preferences}

### 踩坑记录
{pitfalls}

### 用户评估要求
{eval_requirements}
```

#### 1.5 严重程度说明

```
## 严重程度判定

- Critical（必须通知用户）：事实性错误、结论被证伪、违反用户禁忌、重复踩坑
- Warning（自动修正）：非关键建议、风格问题、非最佳实践
```

#### 1.6 输出格式

保持不变：
```
评估结果-正常
评估结果-存在问题。具体问题：1.问题描述及修正建议 [Critical/Warning]
```

### 2. 方法签名扩展（eval_brain.rs）

```rust
pub async fn evaluate(
    &self,
    user_input: &str,
    ai_output: &str,
    turns: &[TurnRecord],
    eval_requirements: &[EvalRequirement],
    user_profile: Option<&UserProfile>,      // 新增：用户画像
    pitfalls: Option<&[PitfallRecord]>,      // 新增：踩坑记录
) -> Result<EvalResult>
```

`build_evaluation_system_prompt` 签名同步扩展：
```rust
pub fn build_evaluation_system_prompt(
    eval_requirements: &[EvalRequirement],
    registry: &SkillRegistry,
    with_tools: bool,
    user_profile: Option<&UserProfile>,      // 新增
    pitfalls: Option<&[PitfallRecord]>,      // 新增
) -> String
```

### 3. 新建 Skills

#### 3.1 troubleshooting-verification.md（排查问题结论验证）

核心规则：
- 主脑给出问题原因 → 必须有日志/链路/数据佐证
- 主脑给出解决方案 → 必须验证方案是否可执行（读相关文件/代码确认）
- 主脑说"可能是XX原因" → 如果无法验证，标记为 Warning
- 主脑说"确定是XX原因" → 如果没有佐证，标记为 Critical
- 搜集到的证据与主脑结论矛盾 → Critical + 证伪说明

#### 3.2 writing-verification.md（写作内容评估）

核心规则：
- 检查是否违反用户写作禁忌（如果用户在会话中定义了写作规则）
- 检查是否有数据支撑（引用了数据是否有来源）
- 检查是否有逻辑自洽性（前后矛盾）
- 不评估文学质量（主观），只评估客观正确性

### 4. 编排器改动（orchestrator.rs）

#### 4.1 加载画像数据

在评估脑调用处，从记忆脑加载 user_profile 和 pitfalls：

```rust
let (eval_requirements, user_profile, pitfalls) = {
    let mem = this.memory_brain.lock().await;
    let reqs = mem.load_eval_requirements();
    let profile = mem.load_user_profile();   // 新增
    let pits = mem.load_active_pitfalls();   // 新增
    (reqs, profile, pits)
};

eb.evaluate(&input, &answer, &turns, &reqs, Some(&profile), Some(&pits)).await
```

#### 4.2 Severity 分层处理

```rust
match eval_result {
    passed => 继续输出
    !passed => {
        if contains_critical_issues(&eval_result.feedback) {
            // Critical: 通知用户，不自动重试
            // 展示评估结果，等待用户决定
        } else {
            // Warning: 自动注入历史，重试（保持现有逻辑）
            brain.push_evaluator_to_history(&eval_result.feedback);
            // 重试...
        }
    }
}
```

#### 4.3 记忆脑新增方法

`PyramidMemoryBrain` 需要暴露两个方法：
- `load_user_profile() -> UserProfile`
- `load_active_pitfalls() -> Vec<PitfallRecord>`

### 5. 改动文件清单

| 文件 | 改动类型 | 改动内容 |
|------|---------|---------|
| `brain-eval/src/prompts.rs` | 重写 | 角色定义 + 技能路由表 + 用户画像注入段 |
| `brain-eval/src/eval_brain.rs` | 扩展 | `evaluate()` 签名 + 传入画像数据 |
| `brain-eval/skills/troubleshooting-verification.md` | 新建 | 排查问题审查规则 |
| `brain-eval/skills/writing-verification.md` | 新建 | 写作内容审查规则 |
| `brain-memory/src/pyramid_memory_brain.rs` | 新增方法 | `load_user_profile()` + `load_active_pitfalls()` |
| `ai-brain-cli/src/orchestrator.rs` | 改动 | 加载画像 + severity 分层 |

### 6. 不动的东西

- `eval_tool_loop` — 完全复用
- `checker.rs` + `quick_check` — 保留作为预检
- `build_read_only_tool_definitions` — 不改
- `SkillRegistry` — 不改
- `EvalResult` / `EvalIssue` 结构 — 不改
- 现有 `code-verification` 和 `conclusion-verification` skills — 保留
