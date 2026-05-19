# 评估脑 Skill 化重构设计

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 将评估脑的硬编码评估逻辑重构为基于 Skills 渐进式披露的架构，支持按任务类型动态加载审查规则，修复日期幻觉和没事找事两个核心问题。

**Architecture:** 评估脑新增 Skill 元工具 + Skill 文件目录。LLM 通过 `<available_skills>` 描述自然路由到对应 skill，按需调用 Skill tool 加载完整规则。环境信息注入修复事实幻觉，Skill 铁律修复过度评估。

**Tech Stack:** Rust, brain-eval crate, SKILL.md (Markdown + YAML frontmatter)

---

## 解决的问题

### P0: 日期幻觉
**现象:** 用户问宁波天气，主脑正确回答，评估脑幻觉"今天是 2025-10-12"然后说主脑时间错了。
**根因:** 评估脑 system prompt 没有注入环境信息（当前日期、OS）。
**修复:** 在评估脑 prompt 中新增 `build_environment_info()`，注入当前日期、OS、工作目录。

### P0: 没事找事
**现象:** 用户问问题，主脑正确回答，评估脑因为"没有叫用户老大"报问题，触发重新跑。
**根因:** 评估维度对所有场景一视同仁，偏好检查无上下文相关性判断。
**修复:** Skill 化后，每个 Skill 有明确的适用场景和铁律（"偏好只在直接相关时检查"）。

---

## Skill 文件结构

```
brain-eval/skills/
├── code-review/
│   └── SKILL.md          # 代码审查技能
├── fact-check/
│   └── SKILL.md          # 事实校验技能
├── task-completion/
│   └── SKILL.md          # 任务完成度技能
├── writing-quality/
│   └── SKILL.md          # 写作质量技能
└── (未来通过 create-skill 动态新增)
```

每个 SKILL.md 格式：
```markdown
---
name: code-review
description: 代码安全性、完整性、最佳实践审查。当主脑输出包含代码修改时适用。
---

# 代码审查

## 检查维度
...

## 铁律
...

## 反合理化表
...
```

## Skill 加载机制（标准渐进式披露）

### 层级 0: available_skills（始终在 system prompt 中）
```xml
<available_skills>
  <skill>
    <name>code-review</name>
    <description>代码安全性、完整性、最佳实践审查</description>
  </skill>
  <skill>
    <name>fact-check</name>
    <description>事实正确性、数据来源、幻觉检测</description>
  </skill>
  ...
</available_skills>
```

### 层级 1: Skill tool 调用（LLM 按需加载）
LLM 调用 `Skill("code-review")` → 工具返回 SKILL.md 完整内容 → LLM 按规则执行评估。

### 层级 2: 验证工具（可选）
加载 Skill 后，LLM 可调用 read_file/grep_search/glob_search/bash 验证。

## 统一后的评估流程

```
主脑输出完成
  ↓
quick_check（规则预检，不调 LLM）
  ├── 发现 Critical 问题 → 直接返回 EvalResult
  └── 无 Critical 问题 → 继续 LLM 评估
  ↓
组装 prompt：
  ├── system: 角色 + 环境信息 + <available_skills> + 踩坑库/画像/规则 + 输出格式
  ├── user: 用户输入 + 主脑输出 + 文件修改记录
  └── tools: Skill + read_file + grep_search + glob_search + bash
  ↓
Round 1: LLM 评估（带工具定义）
  ├── 调用 Skill tool → 获取完整 skill 规则
  ├── 调用 read_file/grep/bash → 获取验证证据
  ├── 不调用任何工具 → 直接出结果
  ↓
有工具调用 → Round 2: 带证据出最终评估（不带工具定义，确保出文本结果）
无工具调用 → Round 1 结果即最终结果
  ↓
返回 EvalResult { passed, feedback }
```

最多 2 轮 LLM 调用，Round 2 一定会出结果（无 tools 可用）。

## 代码改动范围

```
brain-eval/
├── src/
│   ├── lib.rs              ← 新增 pub mod skills
│   ├── eval_brain.rs       ← 重构：合并两条路径 + 新增 Skill/bash 工具
│   ├── prompts.rs          ← 重写：环境信息 + available_skills + 分段组装
│   ├── checker.rs          ← 保留不变
│   ├── extractor.rs        ← 保留不变
│   ├── error.rs            ← 保留不变
│   └── skills/             ← 新增模块
│       ├── mod.rs          ← Skill loader（读目录 + 注册 + 查找）
│       └── types.rs        ← SkillMeta（name, description, content）
├── skills/                 ← 新增：Skill 文件目录
│   ├── code-review/SKILL.md
│   ├── fact-check/SKILL.md
│   ├── task-completion/SKILL.md
│   └── writing-quality/SKILL.md
```

### 核心改动

| 文件 | 改动 | 原因 |
|------|------|------|
| `prompts.rs` | 重写 `build_evaluation_system_prompt` | 加入环境信息 + `<available_skills>` |
| `eval_brain.rs` | 合并 `evaluate` + `evaluate_with_verification` | 统一流程：Skill tool 始终可用 |
| `eval_brain.rs` | 工具定义新增 `Skill` tool + `bash` | 渐进式披露 + 验证能力 |
| `skills/mod.rs` | 新建 Skill loader | 运行时读 SKILL.md，提供 name/description/content |
| `prompts.rs` | 新增 `build_environment_info` | 修复日期幻觉（复用主脑逻辑） |

### 不改的部分

- `checker.rs` — quick_check 规则匹配保留
- `extractor.rs` — 文件变更提取保留
- `brain-core` 类型 — `EvalResult`、`EvalIssue` 等不变
- 编排器调用接口 — `orchestrator.rs` 对 `EvalBrain` 的调用方式不变

## bash 只读命令白名单

```rust
const READ_ONLY_BASH_COMMANDS: &[&str] = &[
    "cargo check",
    "cargo clippy",
    "cargo test",
    "git diff",
    "git log",
    "git status",
    "ls",
    "cat",
    "head",
    "wc",
];
```

执行前校验命令前缀，不匹配则拒绝。

## Skill 内容（占位，后续单独设计）

4 个默认 Skill 的具体检查维度、铁律、反合理化表后续单独设计。
当前先用最小占位内容，确保框架跑通。
