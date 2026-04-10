# Skills 系统探索报告

> 探索日期: 2026-04-09

## 一、Claude Code Skills 概念（联网调研）

### 1.1 Skill 定义

Skill 是一个 **SKILL.md 文件**，包含：
- YAML frontmatter（name, description, trigger 条件）
- Markdown 格式的执行指令（工作流步骤、验证标准、反模式）

### 1.2 核心特征

| 特征 | 说明 |
|------|------|
| **渐进式加载** | 元数据扫描 30-50 tokens/skill，匹配后加载完整内容 |
| **自动激活** | 根据上下文自动匹配触发，不需要手动命令 |
| **可组合** | Skills 之间串联成管道（brainstorming → writing-plans → executing-plans） |
| **跨平台** | 同一 SKILL.md 在 Claude Code、Codex、Gemini CLI 通用 |

### 1.3 Superpowers 框架（40K Star）

七阶段工作流：

```
Stage 1: Brainstorming   — Socratic 提问澄清需求
Stage 2: Git Worktree    — 隔离工作空间
Stage 3: Writing Plans   — 拆解为可执行步骤
Stage 4: Subagent Dev    — 子代理逐任务执行
Stage 5: TDD             — RED-GREEN-REFACTOR 强制循环
Stage 6: Code Review     — 双代理审查
Stage 7: Branch Complete — 验证 + 合并/PR
```

14 个核心技能分四类：
- **测试**: test-driven-development, systematic-debugging, verification-before-completion
- **协作**: brainstorming, writing-plans, executing-plans, dispatching-parallel-agents, requesting-code-review, receiving-code-review, using-git-worktrees, finishing-a-development-branch, subagent-driven-development
- **元技能**: writing-skills, using-superpowers

### 1.4 Skill 执行方式

当 LLM 调用 `Skill` 工具时：
1. `resolve_skill_path()` 搜索多个目录找到 SKILL.md
2. 读取完整 SKILL.md 内容
3. 作为 `SkillOutput.prompt` 返回给 LLM
4. LLM 根据 prompt 内容指导后续行为

搜索路径（优先级从高到低）：
- `$CODEX_HOME/skills/{skill_name}/SKILL.md`
- `$HOME/.agents/skills/{skill_name}/SKILL.md`
- `$HOME/.config/opencode/skills/{skill_name}/SKILL.md`
- `$HOME/.codex/skills/{skill_name}/SKILL.md`

---

## 二、当前工程中的 Skills 实现

### 2.1 代码位置

- Skill 工具定义：`crates/tools/src/lib.rs` (Skill 工具在 mvp_tool_specs 中)
- Skill 执行：`crates/tools/src/lib.rs` 中的 `execute_skill()` 函数
- Skill 命令：`crates/commands/src/lib.rs` 中的 `/skills` 命令
- Skill 发现：`commands/src/lib.rs` 中的 `discover_skill_roots()`

### 2.2 已实现的部分

| 能力 | 实现位置 | 状态 |
|------|---------|------|
| Skill 工具定义（ToolSpec） | `tools/lib.rs` | ✅ 完整（含 input_schema） |
| Skill 执行（读取 SKILL.md 返回给 LLM） | `tools/lib.rs::execute_skill()` | ✅ 完整 |
| Skill 路径搜索 | `tools/lib.rs::resolve_skill_path()` | ✅ 完整 |
| SKILL.md frontmatter 解析 | `commands/src/lib.rs::parse_skill_frontmatter()` | ✅ 完整 |
| `/skills list` 命令 | `commands/src/lib.rs` | ✅ 完整 |
| `/skills install` 命令 | `commands/src/lib.rs` | ✅ 完整 |

### 2.3 未实现的部分

| 能力 | 状态 | 说明 |
|------|------|------|
| 技能自动匹配（根据任务上下文触发） | ❌ 无 | 当前靠 LLM 自己调 `Skill` 工具 |
| 技能索引构建 | ❌ 无 | 每次都扫描文件系统 |
| 技能元数据库 | ❌ 无 | 没有全局的技能摘要缓存 |

### 2.4 与新架构的差距

**感知脑需要的**：根据任务类型和拆解内容，自动匹配推荐 top 5 技能/MCP/插件。

**当前能力**：
- Skill 发现和加载 ✅ 可复用
- Skill frontmatter 解析 ✅ 可复用
- 但没有"根据任务语义匹配技能"的能力，需要新增

**方案**：感知脑调用 LLM 时，将所有可用技能的 name + description 列表作为上下文注入，让 LLM 判断推荐哪些。复用现有的 `discover_skill_roots()` + `parse_skill_frontmatter()` 来构建技能列表。

---

## 三、参考资源

- [Superpowers Deep Dive](https://www.heyuan110.com/posts/ai/2026-02-01-superpowers-deep-dive/)
- [Claude Code Skills vs MCP vs Plugins](https://morphllm.com/claude-code-skills-mcp-plugins)
- [Top 10 Claude Code Skills](https://composio.dev/content/top-claude-skills)
- [Essential Claude Code Skills and Commands](https://batsov.com/articles/2026/03/11/essential-claude-code-skills-and-commands/)
