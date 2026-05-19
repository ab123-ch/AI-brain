# 评估脑 Skill 化重构实现计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 将评估脑的硬编码评估逻辑重构为基于 Skills 渐进式披露的架构，修复日期幻觉和过度评估问题。

**Architecture:** 新增 skills 模块（Skill loader）+ skills 目录（SKILL.md 文件）+ 重写 prompts.rs（环境信息 + available_skills）+ 合并评估路径 + 新增 Skill/bash 工具。

**Tech Stack:** Rust, brain-eval crate, YAML frontmatter 解析

---

## Task 1: 创建 skills 模块基础结构

**Files:**
- Create: `rust/crates/brain-eval/src/skills/mod.rs`
- Create: `rust/crates/brain-eval/src/skills/types.rs`
- Create: `rust/crates/brain-eval/skills/code-review/SKILL.md`
- Create: `rust/crates/brain-eval/skills/fact-check/SKILL.md`
- Create: `rust/crates/brain-eval/skills/task-completion/SKILL.md`
- Create: `rust/crates/brain-eval/skills/writing-quality/SKILL.md`
- Modify: `rust/crates/brain-eval/src/lib.rs:5`

**Step 1: 创建 skills 目录和模块文件**

Create `rust/crates/brain-eval/src/skills/mod.rs`:
```rust
mod types;

pub use types::{SkillMeta, SkillRegistry};
```

Create `rust/crates/brain-eval/src/skills/types.rs`:
```rust
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Skill 元数据（从 SKILL.md frontmatter 解析）
#[derive(Debug, Clone)]
pub struct SkillMeta {
    /// Skill 名称
    pub name: String,
    /// Skill 描述（用于 available_skills 列表）
    pub description: String,
    /// SKILL.md 完整内容（不含 frontmatter）
    pub content: String,
}

/// Skill 注册表（管理所有可用 skill）
#[derive(Debug, Clone, Default)]
pub struct SkillRegistry {
    skills: HashMap<String, SkillMeta>,
}

impl SkillRegistry {
    /// 创建空注册表
    pub fn new() -> Self {
        Self::default()
    }

    /// 从目录加载所有 skill
    ///
    /// 扫描指定目录下所有子目录，读取 SKILL.md 文件
    pub fn load_from_dir(&mut self, dir: &Path) -> std::io::Result<()> {
        if !dir.exists() {
            return Ok(());
        }

        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let skill_dir = entry.path();
            if skill_dir.is_dir() {
                let skill_file = skill_dir.join("SKILL.md");
                if skill_file.exists() {
                    if let Some(meta) = self.parse_skill_file(&skill_file) {
                        self.skills.insert(meta.name.clone(), meta);
                    }
                }
            }
        }
        Ok(())
    }

    /// 解析单个 SKILL.md 文件
    fn parse_skill_file(&self, path: &Path) -> Option<SkillMeta> {
        let content = fs::read_to_string(path).ok()?;
        self.parse_skill_content(&content)
    }

    /// 解析 SKILL.md 内容（frontmatter + body）
    fn parse_skill_content(&self, content: &str) -> Option<SkillMeta> {
        // 提取 YAML frontmatter
        let frontmatter_end = content.find("---\n").and_then(|start| {
            content[start + 4..].find("---\n").map(|end| start + 4 + end + 4)
        })?;

        let frontmatter = &content[..frontmatter_end];
        let body = &content[frontmatter_end..];

        // 解析 name 和 description
        let name = self.extract_yaml_field(frontmatter, "name")?;
        let description = self.extract_yaml_field(frontmatter, "description")?;

        Some(SkillMeta {
            name,
            description,
            content: body.trim().to_string(),
        })
    }

    /// 从 YAML frontmatter 中提取字段值
    fn extract_yaml_field(&self, yaml: &str, field: &str) -> Option<String> {
        let pattern = format!("{field}:");
        for line in yaml.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with(&pattern) {
                let value = trimmed[field.len() + 1..].trim();
                // 移除引号（如果有）
                let value = value.trim_matches('"');
                return Some(value.to_string());
            }
        }
        None
    }

    /// 获取所有 skill 元数据（用于构建 available_skills）
    pub fn all_skills(&self) -> Vec<&SkillMeta> {
        self.skills.values().collect()
    }

    /// 获取指定 skill 的完整内容
    pub fn get_skill_content(&self, name: &str) -> Option<&str> {
        self.skills.get(name).map(|s| s.content.as_str())
    }

    /// 检查 skill 是否存在
    pub fn has_skill(&self, name: &str) -> bool {
        self.skills.contains_key(name)
    }
}
```

**Step 2: 创建 4 个占位 SKILL.md 文件**

Create `rust/crates/brain-eval/skills/code-review/SKILL.md`:
```markdown
---
name: code-review
description: 代码安全性、完整性、最佳实践审查。当主脑输出包含代码修改时适用。
---

# 代码审查

## 检查维度

1. 安全性 — 危险操作（rm -rf、force push、unsafe）
2. 完整性 — TODO/FIXME/省略号占位
3. 正确性 — API 用法、语法（必须用工具验证）

## 铁律

- 只报告 100% 确定的问题
- 不确定的代码模式不报告
- 偏好合规只在代码风格相关时检查
```

Create `rust/crates/brain-eval/skills/fact-check/SKILL.md`:
```markdown
---
name: fact-check
description: 事实正确性、数据来源、幻觉检测。当主脑输出包含事实陈述时适用。
---

# 事实校验

## 检查维度

1. 日期/时间 — 与环境信息对比
2. 技术事实 — 库版本、API 参数

## 铁律

- 你不知道的事实 = 不是问题
- 当前日期以环境信息为准
- 不确定 = 不报告
```

Create `rust/crates/brain-eval/skills/task-completion/SKILL.md`:
```markdown
---
name: task-completion
description: 任务完成度、需求覆盖。始终适用。
---

# 任务完成度

## 检查维度

1. 需求覆盖 — 逐条对比用户要求
2. 遗漏检查 — 用户提到但没回应的点
3. 偏好合规 — 只在直接相关时检查

## 铁律

- 偏好合规 ≠ 无条件检查所有偏好
- 事实性问答不检查格式偏好
```

Create `rust/crates/brain-eval/skills/writing-quality/SKILL.md`:
```markdown
---
name: writing-quality
description: 文本质量、风格、格式。只在用户明确要求写作/文档时适用。
---

# 写作质量

## 检查维度

1. 可读性 — 语句通顺、逻辑连贯
2. 格式 — Markdown 格式正确
3. 风格 — 与用户要求一致

## 铁律

- 不对代码输出检查"写作质量"
- 只在用户明确要求写作时适用
```

**Step 3: 更新 lib.rs 导出 skills 模块**

Modify `rust/crates/brain-eval/src/lib.rs`:
```rust
pub(crate) mod checker;
pub mod error;
pub mod eval_brain;
pub(crate) mod extractor;
pub(crate) mod prompts;
pub mod skills;  // 新增

pub use eval_brain::{EvalBrain, EvalIssue, EvalResult, IssueCategory, IssueSeverity};
```

**Step 4: 运行编译检查**

Run: `cd rust && cargo check -p brain-eval`
Expected: PASS（无错误）

**Step 5: Commit**

```bash
git add rust/crates/brain-eval/src/skills/ rust/crates/brain-eval/skills/ rust/crates/brain-eval/src/lib.rs
git commit -m "feat(eval): 新增 skills 模块和占位 SKILL.md 文件"
```

---

## Task 2: 实现 Skill loader 测试

**Files:**
- Modify: `rust/crates/brain-eval/src/skills/types.rs`（追加测试）

**Step 1: 追加单元测试到 types.rs**

Append to `rust/crates/brain-eval/src/skills/types.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_skill_file(dir: &Path, name: &str, description: &str, content: &str) {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        let skill_md = format!(
            "---\nname: {name}\ndescription: {description}\n---\n\n{content}"
        );
        fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();
    }

    #[test]
    fn parse_skill_content extracts_name_and_description() {
        let registry = SkillRegistry::new();
        let content = "---\nname: test-skill\ndescription: A test skill\n---\n\n# Test Content\n\nSome instructions.";
        let meta = registry.parse_skill_content(content).unwrap();

        assert_eq!(meta.name, "test-skill");
        assert_eq!(meta.description, "A test skill");
        assert!(meta.content.contains("Test Content"));
    }

    #[test]
    fn parse_skill_content_handles_quoted_values() {
        let registry = SkillRegistry::new();
        let content = "---\nname: \"quoted-name\"\ndescription: \"quoted description\"\n---\n\nContent";
        let meta = registry.parse_skill_content(content).unwrap();

        assert_eq!(meta.name, "quoted-name");
        assert_eq!(meta.description, "quoted description");
    }

    #[test]
    fn parse_skill_content_returns_none_without_frontmatter() {
        let registry = SkillRegistry::new();
        let content = "No frontmatter here\nJust content";
        assert!(registry.parse_skill_content(content).is_none());
    }

    #[test]
    fn load_from_dir_loads_all_skills() {
        let tmp_dir = TempDir::new().unwrap();
        create_skill_file(tmp_dir.path(), "skill-a", "Skill A desc", "# Skill A\n\nContent A");
        create_skill_file(tmp_dir.path(), "skill-b", "Skill B desc", "# Skill B\n\nContent B");

        let mut registry = SkillRegistry::new();
        registry.load_from_dir(tmp_dir.path()).unwrap();

        assert_eq!(registry.all_skills().len(), 2);
        assert!(registry.has_skill("skill-a"));
        assert!(registry.has_skill("skill-b"));
    }

    #[test]
    fn load_from_dir_handles_empty_directory() {
        let tmp_dir = TempDir::new().unwrap();
        let mut registry = SkillRegistry::new();
        registry.load_from_dir(tmp_dir.path()).unwrap();

        assert_eq!(registry.all_skills().len(), 0);
    }

    #[test]
    fn load_from_dir_handles_nonexistent_directory() {
        let mut registry = SkillRegistry::new();
        let result = registry.load_from_dir(Path::new("/nonexistent/path"));
        assert!(result.is_ok());
        assert_eq!(registry.all_skills().len(), 0);
    }

    #[test]
    fn get_skill_content_returns_correct_content() {
        let tmp_dir = TempDir::new().unwrap();
        create_skill_file(tmp_dir.path(), "my-skill", "My skill", "# Instructions\n\nDo this.");

        let mut registry = SkillRegistry::new();
        registry.load_from_dir(tmp_dir.path()).unwrap();

        let content = registry.get_skill_content("my-skill").unwrap();
        assert!(content.contains("Instructions"));
        assert!(content.contains("Do this"));
    }
}
```

**Step 2: 添加 tempfile dev-dependency**

Modify `rust/crates/brain-eval/Cargo.toml`:
```toml
[dev-dependencies]
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
tempfile = "3"  # 新增
```

**Step 3: 运行测试验证通过**

Run: `cd rust && cargo test -p brain-eval --lib skills::types::tests`
Expected: PASS（所有 7 个测试通过）

**Step 4: Commit**

```bash
git add rust/crates/brain-eval/src/skills/types.rs rust/crates/brain-eval/Cargo.toml
git commit -m "test(eval): Skill loader 单元测试"
```

---

## Task 3: 重写 prompts.rs（环境信息 + available_skills）

**Files:**
- Modify: `rust/crates/brain-eval/src/prompts.rs`

**Step 1: 新增 build_environment_info 函数**

在 `prompts.rs` 文件顶部新增：
```rust
use std::fmt::Write;

use brain_core::types::{EvalRequirement, EvolutionRule, PitfallRecord, UserProfile};

use crate::extractor::{FileChange, FileChangeType};
use crate::skills::SkillRegistry;

/// 构建运行环境信息段（注入 system prompt）
///
/// 让评估脑感知当前日期，避免幻觉错误时间。
pub fn build_environment_info() -> String {
    let os = match std::env::consts::OS {
        "macos" => "macOS",
        "linux" => "Linux",
        "windows" => "Windows",
        other => other,
    };
    let cwd = std::env::current_dir()
        .map_or_else(|_| "unknown".into(), |p| p.display().to_string());
    let now = chrono::Utc::now();
    let date_str = now.format("%Y年%m月%d日").to_string();
    let weekday = match now.weekday().num_days_from_monday() {
        0 => "周一",
        1 => "周二",
        2 => "周三",
        3 => "周四",
        4 => "周五",
        5 => "周六",
        6 => "周日",
        _ => "未知",
    };
    format!("\n## 运行环境\n- 操作系统: {os}\n- 工作目录: {cwd}\n- 当前日期: {date_str} {weekday}")
}
```

**Step 2: 新增 build_available_skills 函数**

```rust
/// 构建 available_skills XML 段
///
/// 将所有 skill 的 name + description 格式化为 XML，
/// 嵌入 system prompt，供 LLM 自然路由。
pub fn build_available_skills(registry: &SkillRegistry) -> String {
    let skills = registry.all_skills();
    if skills.is_empty() {
        return String::new();
    }

    let mut s = String::from("\n<available_skills>\n");
    for skill in skills {
        let _ = writeln!(s, "  <skill>");
        let _ = writeln!(s, "    <name>{}</name>", skill.name);
        let _ = writeln!(s, "    <description>{}</description>", skill.description);
        let _ = writeln!(s, "  </skill>");
    }
    s.push_str("</available_skills>\n");
    s
}
```

**Step 3: 重写 build_evaluation_system_prompt 函数**

将原有的 `build_evaluation_system_prompt` 函数改为：
```rust
/// 构建评估系统提示词（Skill 化版本）
///
/// 四段式结构：角色定义 + 环境信息 + available_skills + 用户评估要求 + 输出格式
pub fn build_evaluation_system_prompt(
    eval_requirements: &[EvalRequirement],
    registry: &SkillRegistry,
    with_tools: bool,
) -> String {
    let mut prompt = String::new();

    // ── 第一段：角色定义 ──
    prompt.push_str(r#"# 角色定义

你是 AI Brain 系统的质量审核员，负责在主脑产生输出后评估其质量和安全性。

## 核心能力
- **任务完成度审查**：判断主脑是否真正完成了用户要求的任务
- **安全风险识别**：检测代码中的危险操作和安全隐患
- **事实正确性校验**：验证日期、技术细节是否准确（以环境信息为准）
- **偏好合规检查**：确认输出遵守用户的偏好和禁忌（只在直接相关时）

## 工作方式
- 你可以调用 Skill 工具加载具体的审查规则
- 你可以使用只读工具（read_file、grep、bash）验证代码
- 只报告确实存在的问题，宁可漏报不误报
- 不确定的事实不要标记为错误

"#);

    // ── 第二段：环境信息（修复日期幻觉） ──
    prompt.push_str(&build_environment_info());
    prompt.push('\n');

    // ── 第三段：available_skills（渐进式披露层级 0） ──
    prompt.push_str(&build_available_skills(registry));

    // ── Skill tool 说明 ──
    if with_tools {
        prompt.push_str(r#"# Skill 工具

你可以调用 Skill 工具加载具体审查规则：
- `Skill("code-review")` — 代码审查规则
- `Skill("fact-check")` — 事实校验规则
- `Skill("task-completion")` — 任务完成度规则
- `Skill("writing-quality")` — 写作质量规则

加载后你将看到完整的检查维度和铁律。

"#);
    }

    // ── 第四段：用户评估要求（动态积累） ──
    if !eval_requirements.is_empty() {
        prompt.push_str("# 用户评估要求\n\n");
        prompt.push_str("以下是用户对评估的具体要求和纠正，请严格遵循：\n\n");
        for (i, req) in eval_requirements.iter().enumerate() {
            let _ = writeln!(prompt, "{}. {}", i + 1, req.content);
        }
        prompt.push_str("\n**用户评估要求优先级高于固定评估维度。**\n\n");
    }

    // ── 输出格式 ──
    prompt.push_str(r#"# 输出格式

没有问题时，严格输出（不要附加其他文字）：
评估结果-正常

有问题时，严格输出（不要附加其他文字）：
评估结果-存在问题。具体问题：1.问题描述及修正建议 2.问题描述及修正建议 ...

注意：不要输出 JSON，不要使用代码块，只输出纯文本。"#);

    prompt
}
```

**Step 4: 保留原有的 build_evaluation_user_prompt 函数（不变）**

**Step 5: 运行编译检查**

Run: `cd rust && cargo check -p brain-eval`
Expected: PASS

**Step 6: 运行原有测试验证通过**

Run: `cd rust && cargo test -p brain-eval --lib prompts::tests`
Expected: 部分测试可能失败（因为函数签名变了），记录失败情况

**Step 7: Commit**

```bash
git add rust/crates/brain-eval/src/prompts.rs
git commit -m "refactor(eval): prompts 重写，新增环境信息和 available_skills"
```

---

## Task 4: 新增 Skill tool 和 bash 只读白名单

**Files:**
- Modify: `rust/crates/brain-eval/src/eval_brain.rs`

**Step 1: 新增 bash 只读命令白名单**

在 `eval_brain.rs` 中的 `READ_ONLY_TOOLS` 常量后新增：
```rust
/// 评估脑允许使用的只读工具白名单
const READ_ONLY_TOOLS: &[&str] = &["read_file", "grep_search", "glob_search", "Skill"];

/// bash 只读命令白名单（前缀匹配）
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

/// 检查 bash 命令是否在只读白名单中
pub(crate) fn is_read_only_bash_command(cmd: &str) -> bool {
    let cmd_lower = cmd.trim().to_lowercase();
    READ_ONLY_BASH_COMMANDS.iter().any(|allowed| {
        cmd_lower.starts_with(&allowed.to_lowercase())
    })
}
```

**Step 2: 新增 Skill tool 定义**

在 `build_read_only_tool_definitions()` 函数中追加 Skill 和 bash 工具：
```rust
/// 构建评估脑专用的只读工具定义（含 Skill + bash）
pub fn build_read_only_tool_definitions(skills_dir: &str) -> Vec<ToolDefinition> {
    vec![
        // Skill tool
        ToolDefinition {
            name: "Skill".into(),
            description: format!("加载审查技能的完整规则。\n\n可用技能：code-review, fact-check, task-completion, writing-quality\n\n技能目录: {skills_dir}").into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "要加载的技能名称，如 code-review 或 fact-check"
                    }
                },
                "required": ["command"]
            }),
        },
        // read_file
        ToolDefinition {
            name: "read_file".into(),
            description: "读取文件内容（只读）。可以指定行范围。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "要读取的文件的绝对路径"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "从第几行开始读取（可选）"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "最多读取多少行（可选）"
                    }
                },
                "required": ["file_path"]
            }),
        },
        // grep_search
        ToolDefinition {
            name: "grep_search".into(),
            description: "在文件内容中搜索匹配正则表达式的行（只读）。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "正则表达式搜索模式"
                    },
                    "path": {
                        "type": "string",
                        "description": "搜索目录（可选）"
                    },
                    "output_mode": {
                        "type": "string",
                        "description": "输出模式：content 或 files_with_matches"
                    },
                    "head_limit": {
                        "type": "integer",
                        "description": "最多返回多少条结果"
                    }
                },
                "required": ["pattern"]
            }),
        },
        // glob_search
        ToolDefinition {
            name: "glob_search".into(),
            description: "按 glob 模式搜索文件路径（只读）。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "glob 模式，如 **/*.rs"
                    },
                    "path": {
                        "type": "string",
                        "description": "搜索目录（可选）"
                    }
                },
                "required": ["pattern"]
            }),
        },
        // bash（只读）
        ToolDefinition {
            name: "bash".into(),
            description: "执行只读 shell 命令（如 cargo check、git diff）。只允许白名单命令。".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "要执行的只读命令（必须在白名单中）"
                    }
                },
                "required": ["command"]
            }),
        },
    ]
}
```

**Step 3: 更新 is_read_only_tool 函数**

```rust
/// 检查工具名是否在只读白名单中
pub(crate) fn is_read_only_tool(name: &str) -> bool {
    READ_ONLY_TOOLS.contains(&name)
}
```

**Step 4: 运行编译检查**

Run: `cd rust && cargo check -p brain-eval`
Expected: PASS

**Step 5: Commit**

```bash
git add rust/crates/brain-eval/src/eval_brain.rs
git commit -m "feat(eval): 新增 Skill tool 和 bash 只读白名单"
```

---

## Task 5: 合并评估路径 + 实现统一流程

**Files:**
- Modify: `rust/crates/brain-eval/src/eval_brain.rs`

**Step 1: 新增 SkillRegistry 字段到 EvalBrain**

修改 `EvalBrain` struct：
```rust
/// v2 评估脑 — Skill 化版本
pub struct EvalBrain {
    llm: Arc<dyn LlmProvider>,
    tool_executor: Option<Arc<dyn ToolExecutor>>,
    skill_registry: SkillRegistry,
    skills_dir: String,
    progress_tx: Option<tokio::sync::mpsc::Sender<ProgressEvent>>,
}
```

**Step 2: 更新构造函数**

```rust
impl EvalBrain {
    /// 创建评估脑实例
    pub fn new(llm: Arc<dyn LlmProvider>) -> Self {
        let mut registry = SkillRegistry::new();
        // 尝试从 crate 目录加载 skills
        let skills_dir = "rust/crates/brain-eval/skills";
        if let Err(e) = registry.load_from_dir(std::path::Path::new(skills_dir)) {
            tracing::warn!("加载 skills 失败: {e}");
        }
        Self {
            llm,
            tool_executor: None,
            skill_registry: registry,
            skills_dir: skills_dir.into(),
            progress_tx: None,
        }
    }

    /// 创建带工具验证能力的评估脑实例
    pub fn with_verification(
        llm: Arc<dyn LlmProvider>,
        tool_executor: Arc<dyn ToolExecutor>,
    ) -> Self {
        let mut registry = SkillRegistry::new();
        let skills_dir = "rust/crates/brain-eval/skills";
        if let Err(e) = registry.load_from_dir(std::path::Path::new(skills_dir)) {
            tracing::warn!("加载 skills 失败: {e}");
        }
        Self {
            llm,
            tool_executor: Some(tool_executor),
            skill_registry: registry,
            skills_dir: skills_dir.into(),
            progress_tx: None,
        }
    }

    // ... 其他方法保持不变
}
```

**Step 3: 重写 evaluate 方法为统一流程**

将原有的 `evaluate` 和 `evaluate_with_verification` 合并为：
```rust
    /// 统一评估入口
    ///
    /// 流程：quick_check → LLM 评估（带 Skill + 验证工具）
    pub async fn evaluate(
        &self,
        user_input: &str,
        ai_output: &str,
        turns: &[TurnRecord],
        pitfalls: &[PitfallRecord],
        user_profile: &UserProfile,
        rules: &[EvolutionRule],
        eval_requirements: &[EvalRequirement],
    ) -> Result<EvalResult> {
        // 空输入检查
        if user_input.trim().is_empty() || ai_output.trim().is_empty() {
            return Err(EvalError::InvalidInput(
                "user_input and ai_output must not be empty".into(),
            ));
        }

        // 发送评估开始事件
        self.emit_start();

        // Step 1: quick_check 规则预检
        let quick_issues = self.quick_check(ai_output, pitfalls, &user_profile.taboos);
        if quick_issues.iter().any(|i| i.severity == IssueSeverity::Critical) {
            let feedback = format!(
                "评估结果-存在问题。具体问题：{}",
                quick_issues.iter()
                    .filter(|i| i.severity == IssueSeverity::Critical)
                    .map(|i| i.description.clone())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            self.emit_result(false, &feedback);
            return Ok(EvalResult { passed: false, feedback });
        }

        // Step 2: 提取文件变更
        let file_changes = extractor::extract_file_changes(turns);
        let has_tools = self.tool_executor.is_some();

        // Step 3: 组装 prompt
        let system_prompt = prompts::build_evaluation_system_prompt(
            eval_requirements,
            &self.skill_registry,
            has_tools,
        );
        let user_prompt = prompts::build_evaluation_user_prompt(
            user_input,
            ai_output,
            pitfalls,
            user_profile,
            rules,
            &file_changes,
        );

        // Step 4: Round 1 - 带工具定义
        let tools = if has_tools {
            Some(build_read_only_tool_definitions(&self.skills_dir))
        } else {
            None
        };

        let messages = vec![
            ChatMessage::system(&system_prompt),
            ChatMessage::user(&user_prompt),
        ];

        let request = ChatRequest {
            model: None,
            messages,
            max_tokens: Some(2048),
            temperature: Some(0.1),
            tools,
            tool_choice: Some(ToolChoice::Auto),
        };

        let response = self
            .llm
            .complete(request)
            .await
            .map_err(|e| EvalError::LlmError(e.to_string()))?;

        // 无工具调用 → 直接返回
        if !response.has_tool_calls() {
            let feedback = response.text();
            let passed = !feedback.contains("存在问题");
            self.emit_result(passed, &feedback);
            return Ok(EvalResult {
                passed,
                feedback: feedback.trim().to_string(),
            });
        }

        // Step 5: 执行工具调用
        let tool_executor = self.tool_executor.as_ref().ok_or_else(|| {
            EvalError::InvalidInput("tool_executor not available".into())
        })?;

        let mut messages = vec![
            ChatMessage::system(&system_prompt),
            ChatMessage::user(&user_prompt),
            ChatMessage::assistant_blocks(response.content.clone()),
        ];

        for tool_block in response.tool_calls() {
            if let ContentBlock::ToolUse { id, name, input } = tool_block {
                // 安全检查
                if !is_read_only_tool(&name) {
                    tracing::warn!("评估脑工具安全拒绝: {name}");
                    messages.push(ChatMessage::tool_result(
                        id,
                        format!("工具 {name} 不可用：评估脑只允许只读工具"),
                        true,
                    ));
                    continue;
                }

                // Skill tool 特殊处理
                if name == "Skill" {
                    let skill_name = input.get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let content = self.skill_registry
                        .get_skill_content(skill_name)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("Skill '{skill_name}' 不存在"));
                    messages.push(ChatMessage::tool_result(id, content, false));
                    continue;
                }

                // bash 命令安全检查
                if name == "bash" {
                    let cmd = input.get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !is_read_only_bash_command(cmd) {
                        tracing::warn!("评估脑 bash 命令安全拒绝: {cmd}");
                        messages.push(ChatMessage::tool_result(
                            id,
                            format!("命令 '{cmd}' 不在只读白名单中"),
                            true,
                        ));
                        continue;
                    }
                }

                // 执行工具
                let tool_call = ToolCall {
                    tool_name: name.clone(),
                    input: input.clone(),
                    validated: false,
                    validation_id: None,
                };

                tracing::info!("评估脑验证工具: {name}");
                let result = tool_executor.execute(&tool_call).await;
                let output = truncate_verification_output(&result.output, 5000);
                messages.push(ChatMessage::tool_result(id, output, result.is_error));
            }
        }

        // Step 6: Round 2 - 不带工具定义，出最终结果
        let request = ChatRequest {
            model: None,
            messages,
            max_tokens: Some(2048),
            temperature: Some(0.1),
            tools: None,
            tool_choice: None,
        };

        let response = self
            .llm
            .complete(request)
            .await
            .map_err(|e| EvalError::LlmError(e.to_string()))?;

        let feedback = response.text();
        if feedback.trim().is_empty() {
            self.emit_result(true, "评估结果-正常");
            return Ok(EvalResult::passed());
        }

        let passed = !feedback.contains("存在问题");
        self.emit_result(passed, &feedback);
        Ok(EvalResult {
            passed,
            feedback: feedback.trim().to_string(),
        })
    }
```

**Step 4: 删除原有的 evaluate_with_verification 方法**

**Step 5: 更新 emit_start 方法**

```rust
    /// 发送评估开始事件
    fn emit_start(&self) {
        if let Some(tx) = &self.progress_tx {
            let _ = tx.try_send(ProgressEvent::EvaluationStart);
        }
    }
```

**Step 6: 运行编译检查**

Run: `cd rust && cargo check -p brain-eval`
Expected: PASS

**Step 7: Commit**

```bash
git add rust/crates/brain-eval/src/eval_brain.rs
git commit -m "refactor(eval): 合并评估路径，实现统一 Skill 化流程"
```

---

## Task 6: 更新编排器调用

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs:742`

**Step 1: 更新 evaluate_with_verification 调用为 evaluate**

找到编排器中调用评估脑的地方，将：
```rust
.evaluate_with_verification(...)
```
改为：
```rust
.evaluate(...)
```

并确保传入 turns 参数（原来可能没有传入）。

**Step 2: 运行编译检查**

Run: `cd rust && cargo check -p ai-brain-cli`
Expected: PASS

**Step 3: Commit**

```bash
git add rust/crates/ai-brain-cli/src/orchestrator.rs
git commit -m "refactor(orchestrator): 更新评估脑调用接口"
```

---

## Task 7: 运行完整测试验证

**Step 1: 运行 brain-eval 全部测试**

Run: `cd rust && cargo test -p brain-eval`
Expected: 部分测试可能需要更新（原有测试函数签名变了）

**Step 2: 运行 workspace 测试**

Run: `cd rust && cargo test --workspace --exclude brain-integration-tests`
Expected: PASS（如果有失败，逐个修复）

**Step 3: 运行 clippy**

Run: `cd rust && cargo clippy -p brain-eval -- -D warnings`
Expected: PASS（无警告）

**Step 4: 最终 Commit**

```bash
git add -A
git commit -m "test(eval): 验证 Skill 化重构完成"
```

---

## 验证清单

- [ ] Skill loader 能正确解析 SKILL.md frontmatter
- [ ] 环境信息（当前日期）注入到 system prompt
- [ ] `<available_skills>` 正确生成 XML 格式
- [ ] Skill tool 能返回正确的 skill 内容
- [ ] bash 只读白名单生效
- [ ] 统一评估流程跑通（两轮 LLM）
- [ ] 原有 quick_check 规则匹配保留
- [ ] 编排器调用接口正确
- [ ] 所有测试通过
- [ ] clippy 无警告