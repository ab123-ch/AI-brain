# 智脑原生 Skill/Plugin/MCP 系统 — 实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 让智脑原生支持 Skill 技能加载、插件管理、MCP 服务连接，适配现有技能市场和 MCP 服务生态

**Architecture:** 分层架构 — SkillLoader（SKILL.md 解析 + 命名空间）+ PluginManager（plugin.json 清单 + 安装/卸载）+ McpClientPool（rmcp 客户端连接池）。三层通过统一 ToolProvider trait 暴露给主脑 RealToolExecutor。

**Tech Stack:** Rust, rmcp crate (MCP SDK), serde/serde_json, tokio async runtime

**设计文档:** `docs/plans/2026-05-20-native-plugin-system-design.md`

---

## Phase 1: brain-plugin crate（SkillLoader + PluginManager）

### Task 1: 创建 brain-plugin crate 骨架

**Files:**
- Create: `rust/crates/brain-plugin/Cargo.toml`
- Create: `rust/crates/brain-plugin/src/lib.rs`
- Modify: `rust/crates/ai-brain-cli/Cargo.toml` — 添加 `brain-plugin` 依赖

**Step 1: 创建 Cargo.toml**

```toml
[package]
name = "brain-plugin"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
brain-core = { path = "../brain-core" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
tracing = "0.1"

[dev-dependencies]
tempfile = "3"

[lints]
workspace = true
```

**Step 2: 创建 src/lib.rs 模块声明**

```rust
pub mod skill_loader;
pub mod plugin_manager;

pub use skill_loader::{SkillCatalog, SkillMeta};
pub use plugin_manager::{PluginManager, PluginMeta, PluginSource};
```

**Step 3: 在 ai-brain-cli/Cargo.toml 添加依赖**

在 `[dependencies]` 区块中添加:
```toml
brain-plugin = { path = "../brain-plugin" }
```

**Step 4: 验证编译**

Run: `cd rust && cargo check -p brain-plugin`
Expected: 编译通过（空模块会报错，先创建占位文件）

**Step 5: 创建占位文件**

```rust
// src/skill_loader.rs
pub struct SkillMeta;
pub struct SkillCatalog;
```

```rust
// src/plugin_manager.rs
pub struct PluginManager;
pub struct PluginMeta;
pub struct PluginSource;
```

**Step 6: 验证编译**

Run: `cd rust && cargo check -p brain-plugin`
Expected: PASS

**Step 7: Commit**

```bash
git add rust/crates/brain-plugin/
git commit -m "feat(plugin): 创建 brain-plugin crate 骨架"
```

---

### Task 2: 实现 SKILL.md 解析器

**Files:**
- Modify: `rust/crates/brain-plugin/src/skill_loader.rs`

**Step 1: 写测试 — frontmatter 解析**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_skill_md(name: &str, description: &str, body: &str) -> String {
        format!(
            "---\nname: {name}\ndescription: \"{description}\"\n---\n\n{body}"
        )
    }

    #[test]
    fn parse_skill_frontmatter_basic() {
        let content = make_skill_md(
            "brainstorming",
            "Force brainstorming before implementation",
            "# Brainstorming\nDo stuff.",
        );
        let result = parse_skill_content(&content).unwrap();
        assert_eq!(result.name, "brainstorming");
        assert_eq!(result.description, "Force brainstorming before implementation");
        assert!(result.body.contains("# Brainstorming"));
    }

    #[test]
    fn parse_skill_no_frontmatter_returns_none() {
        let content = "# Just markdown\nNo frontmatter.";
        assert!(parse_skill_content(content).is_none());
    }

    #[test]
    fn parse_skill_missing_name_returns_none() {
        let content = "---\ndescription: \"has desc but no name\"\n---\nbody";
        assert!(parse_skill_content(content).is_none());
    }

    #[test]
    fn parse_skill_with_when_to_use() {
        let content = "---\nname: tdd\nwhen_to_use: \"when writing code\"\n---\n# TDD";
        let result = parse_skill_content(&content).unwrap();
        assert_eq!(result.name, "tdd");
        assert_eq!(result.when_to_use, Some("when writing code".to_string()));
    }

    #[test]
    fn parse_skill_quoted_values() {
        let content = "---\nname: 'my-skill'\ndescription: \"A skill\"\n---\nbody";
        let result = parse_skill_content(&content).unwrap();
        assert_eq!(result.name, "my-skill");
    }

    #[test]
    fn scan_skill_dir_discovers_skills() {
        let dir = TempDir::new().unwrap();
        let skill_dir = dir.path().join("brainstorming");
        fs::create_dir(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            make_skill_md("brainstorming", "desc", "# Body"),
        ).unwrap();

        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(catalog.skills[0].name, "brainstorming");
    }

    #[test]
    fn scan_empty_dir_ok() {
        let dir = TempDir::new().unwrap();
        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        assert!(catalog.skills.is_empty());
    }

    #[test]
    fn scan_nonexistent_dir_ok() {
        let catalog = SkillCatalog::scan_all(&[PathBuf::from("/tmp/does-not-exist-xyz")]).unwrap();
        assert!(catalog.skills.is_empty());
    }

    #[test]
    fn resolve_by_name() {
        let catalog = SkillCatalog {
            skills: vec![
                SkillMeta {
                    name: "brainstorming".into(),
                    namespace: None,
                    description: "desc".into(),
                    when_to_use: None,
                    source_path: PathBuf::from("/fake/SKILL.md"),
                },
            ],
        };
        assert!(catalog.resolve("brainstorming").is_some());
        assert!(catalog.resolve("nonexistent").is_none());
    }

    #[test]
    fn resolve_with_namespace() {
        let catalog = SkillCatalog {
            skills: vec![
                SkillMeta {
                    name: "brainstorming".into(),
                    namespace: Some("superpowers".into()),
                    description: "desc".into(),
                    when_to_use: None,
                    source_path: PathBuf::from("/fake/SKILL.md"),
                },
            ],
        };
        assert!(catalog.resolve("superpowers:brainstorming").is_some());
        // 无冒号时也能找到
        assert!(catalog.resolve("brainstorming").is_some());
    }

    #[test]
    fn summary_for_prompt_xml() {
        let catalog = SkillCatalog {
            skills: vec![
                SkillMeta {
                    name: "brainstorming".into(),
                    namespace: None,
                    description: "Force brainstorming".into(),
                    when_to_use: None,
                    source_path: PathBuf::from("/fake"),
                },
            ],
        };
        let xml = catalog.summary_for_prompt();
        assert!(xml.contains("<available_skills>"));
        assert!(xml.contains("<name>brainstorming</name>"));
    }

    #[test]
    fn load_content_reads_body() {
        let dir = TempDir::new().unwrap();
        let skill_dir = dir.path().join("tdd");
        fs::create_dir(&skill_dir).unwrap();
        let skill_path = skill_dir.join("SKILL.md");
        fs::write(
            &skill_path,
            make_skill_md("tdd", "Test driven", "# TDD Rules\nWrite tests first."),
        ).unwrap();

        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        let content = catalog.load_content(&catalog.skills[0]).unwrap();
        assert!(content.contains("# TDD Rules"));
        assert!(!content.contains("name: tdd")); // body only, no frontmatter
    }
}
```

**Step 2: 运行测试确认失败**

Run: `cd rust && cargo test -p brain-plugin`
Expected: FAIL — 函数/类型未定义

**Step 3: 实现解析器核心**

```rust
use std::path::{Path, PathBuf};

/// Skill 元数据（从 SKILL.md frontmatter 解析）
#[derive(Debug, Clone)]
pub struct SkillMeta {
    pub name: String,
    pub namespace: Option<String>,
    pub description: String,
    pub when_to_use: Option<String>,
    pub source_path: PathBuf,
}

/// Skill 解析结果（内部用）
struct ParsedSkill {
    name: String,
    description: String,
    when_to_use: Option<String>,
    body: String,
}

/// Skill 目录（启动时扫描，运行时查询）
#[derive(Debug, Clone)]
pub struct SkillCatalog {
    pub skills: Vec<SkillMeta>,
}

impl SkillCatalog {
    /// 扫描多个根目录，收集所有 SKILL.md 的元数据
    pub fn scan_all(roots: &[PathBuf]) -> Result<Self, String> {
        let mut skills = Vec::new();

        for root in roots {
            if !root.exists() {
                continue;
            }
            if let Ok(entries) = std::fs::read_dir(root) {
                for entry in entries.flatten() {
                    let skill_dir = entry.path();
                    if !skill_dir.is_dir() {
                        continue;
                    }
                    let skill_file = skill_dir.join("SKILL.md");
                    if !skill_file.exists() {
                        continue;
                    }
                    if let Some(parsed) = parse_skill_file(&skill_file) {
                        // 从路径推断 namespace：如果路径包含 "plugins/cache/"，
                        // 取 plugins/cache/{publisher}/{plugin}/ 后的段作为 namespace
                        let namespace = infer_namespace(&skill_file);
                        skills.push(SkillMeta {
                            name: parsed.name,
                            namespace,
                            description: parsed.description,
                            when_to_use: parsed.when_to_use,
                            source_path: skill_file,
                        });
                    }
                }
            }
        }

        Ok(Self { skills })
    }

    /// 解析查询：支持 "name" 和 "namespace:name" 格式
    pub fn resolve(&self, query: &str) -> Option<&SkillMeta> {
        if let Some((ns, name)) = query.split_once(':') {
            // 带命名空间：精确匹配
            self.skills.iter().find(|s| {
                s.namespace.as_deref() == Some(ns) && s.name == name
            })
        } else {
            // 无命名空间：先查无 namespace 的，再查有 namespace 的
            self.skills.iter().find(|s| s.namespace.is_none() && s.name == query)
                .or_else(|| self.skills.iter().find(|s| s.name == query))
        }
    }

    /// 读取 SKILL.md 的 body（不含 frontmatter）
    pub fn load_content(&self, meta: &SkillMeta) -> Result<String, String> {
        let content = std::fs::read_to_string(&meta.source_path)
            .map_err(|e| format!("读取 SKILL.md 失败: {e}"))?;
        let parsed = parse_skill_content(&content)
            .ok_or_else(|| "解析 SKILL.md 失败".to_string())?;
        Ok(parsed.body)
    }

    /// 生成 <available_skills> XML 段，注入 system prompt
    pub fn summary_for_prompt(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }
        let mut s = String::from("\n<available_skills>\n");
        for skill in &self.skills {
            let _ = writeln!(s, "  <skill>");
            let display = if let Some(ref ns) = skill.namespace {
                format!("{ns}:{}", skill.name)
            } else {
                skill.name.clone()
            };
            let _ = writeln!(s, "    <name>{display}</name>");
            let _ = writeln!(s, "    <description>{}</description>", skill.description);
            if let Some(ref when) = skill.when_to_use {
                let _ = writeln!(s, "    <when_to_use>{when}</when_to_use>");
            }
            let _ = writeln!(s, "  </skill>");
        }
        s.push_str("</available_skills>\n");
        s
    }
}

/// 从文件路径推断命名空间
/// 路径格式: .../plugins/cache/{publisher}/{plugin}/{version}/skills/{name}/SKILL.md
fn infer_namespace(path: &Path) -> Option<String> {
    let mut components = path.components().rev();
    // SKILL.md -> {name}/ -> skills/ -> {version}/ -> {plugin}/
    components.next(); // SKILL.md
    components.next(); // {name}
    components.next(); // skills
    let plugin = components.next()?.as_os_str().to_string_lossy().to_string();
    // 检查上面是否有 cache
    if components.next().is_some() {
        let maybe_cache = components.next();
        if let Some(c) = maybe_cache {
            if c.as_os_str() == "cache" {
                return Some(plugin);
            }
        }
    }
    None
}

/// 解析 SKILL.md 文件
fn parse_skill_file(path: &Path) -> Option<ParsedSkill> {
    let content = std::fs::read_to_string(path).ok()?;
    parse_skill_content(&content)
}

/// 解析 SKILL.md 内容：frontmatter + body
fn parse_skill_content(content: &str) -> Option<ParsedSkill> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }

    let after_first = &trimmed[3..];
    let rest = after_first.trim_start_matches(|c: char| c == '\n' || c == '\r');
    let end_pos = rest.find("\n---").or_else(|| rest.find("\r\n---"))?;

    let frontmatter = &rest[..end_pos];
    let body_start = end_pos + 3; // skip "---"
    let body = rest[body_start..].trim_start_matches(|c: char| c == '\n' || c == '\r')
        .trim()
        .to_string();

    let name = extract_field(frontmatter, "name")?;
    let description = extract_field(frontmatter, "description")?;
    let when_to_use = extract_field(frontmatter, "when_to_use");

    Some(ParsedSkill { name, description, when_to_use, body })
}

/// 从 YAML frontmatter 提取字段值
fn extract_field(yaml: &str, field: &str) -> Option<String> {
    let prefix = format!("{field}:");
    for line in yaml.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix(&prefix) {
            let value = value.trim();
            // 去除引号
            let value = value
                .strip_prefix('"').and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}
```

**Step 4: 运行测试确认通过**

Run: `cd rust && cargo test -p brain-plugin`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add rust/crates/brain-plugin/
git commit -m "feat(plugin): 实现 SKILL.md 解析器和 SkillCatalog"
```

---

### Task 3: 实现 PluginManager

**Files:**
- Modify: `rust/crates/brain-plugin/src/plugin_manager.rs`

**Step 1: 写测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_plugin_json(name: &str, version: &str) -> String {
        format!(r#"{{
            "name": "{name}",
            "version": "{version}",
            "description": "Test plugin",
            "skills": "./skills/"
        }}"#)
    }

    fn make_skill_md(name: &str, desc: &str) -> String {
        format!("---\nname: {name}\ndescription: \"{desc}\"\n---\n# {name}")
    }

    #[test]
    fn load_empty_plugins_dir() {
        let dir = TempDir::new().unwrap();
        let mgr = PluginManager::load(dir.path()).unwrap();
        assert!(mgr.list().is_empty());
    }

    #[test]
    fn load_with_no_registry_file() {
        let dir = TempDir::new().unwrap();
        // plugins/ 目录存在但没有 registry.json
        fs::create_dir_all(dir.path().join("cache")).unwrap();
        let mgr = PluginManager::load(dir.path()).unwrap();
        assert!(mgr.list().is_empty());
    }

    #[test]
    fn install_and_list_plugin() {
        let plugins_dir = TempDir::new().unwrap();
        let source_dir = TempDir::new().unwrap();

        // 创建源插件
        let skills_dir = source_dir.path().join("skills").join("tdd");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(source_dir.path().join("plugin.json"), make_plugin_json("test-plugin", "1.0.0")).unwrap();
        fs::write(skills_dir.join("SKILL.md"), make_skill_md("tdd", "TDD skill")).unwrap();

        // 安装
        let mut mgr = PluginManager::load(plugins_dir.path()).unwrap();
        let name = mgr.install(source_dir.path(), "test-publisher").unwrap();
        assert_eq!(name, "test-plugin");

        // 列出
        let plugins = mgr.list();
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "test-plugin");
        assert_eq!(plugins[0].version, "1.0.0");
    }

    #[test]
    fn skill_roots_after_install() {
        let plugins_dir = TempDir::new().unwrap();
        let source_dir = TempDir::new().unwrap();

        let skills_dir = source_dir.path().join("skills").join("tdd");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(source_dir.path().join("plugin.json"), make_plugin_json("test-plugin", "1.0.0")).unwrap();
        fs::write(skills_dir.join("SKILL.md"), make_skill_md("tdd", "TDD skill")).unwrap();

        let mut mgr = PluginManager::load(plugins_dir.path()).unwrap();
        mgr.install(source_dir.path(), "test-publisher").unwrap();

        let roots = mgr.skill_roots();
        assert_eq!(roots.len(), 1);
        assert!(roots[0].to_string_lossy().contains("skills"));
    }

    #[test]
    fn uninstall_removes_plugin() {
        let plugins_dir = TempDir::new().unwrap();
        let source_dir = TempDir::new().unwrap();

        let skills_dir = source_dir.path().join("skills").join("tdd");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(source_dir.path().join("plugin.json"), make_plugin_json("test-plugin", "1.0.0")).unwrap();
        fs::write(skills_dir.join("SKILL.md"), make_skill_md("tdd", "TDD skill")).unwrap();

        let mut mgr = PluginManager::load(plugins_dir.path()).unwrap();
        mgr.install(source_dir.path(), "test-publisher").unwrap();
        assert_eq!(mgr.list().len(), 1);

        mgr.uninstall("test-plugin").unwrap();
        assert!(mgr.list().is_empty());
    }

    #[test]
    fn install_missing_plugin_json_fails() {
        let plugins_dir = TempDir::new().unwrap();
        let source_dir = TempDir::new().unwrap();
        // 没有 plugin.json
        let mut mgr = PluginManager::load(plugins_dir.path()).unwrap();
        assert!(mgr.install(source_dir.path(), "pub").is_err());
    }

    #[test]
    fn reload_persistent_registry() {
        let plugins_dir = TempDir::new().unwrap();
        let source_dir = TempDir::new().unwrap();

        let skills_dir = source_dir.path().join("skills").join("tdd");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(source_dir.path().join("plugin.json"), make_plugin_json("test-plugin", "1.0.0")).unwrap();
        fs::write(skills_dir.join("SKILL.md"), make_skill_md("tdd", "TDD skill")).unwrap();

        // 安装后卸载 mgr
        {
            let mut mgr = PluginManager::load(plugins_dir.path()).unwrap();
            mgr.install(source_dir.path(), "test-publisher").unwrap();
        }

        // 重新加载
        let mgr2 = PluginManager::load(plugins_dir.path()).unwrap();
        assert_eq!(mgr2.list().len(), 1);
    }
}
```

**Step 2: 运行测试确认失败**

Run: `cd rust && cargo test -p brain-plugin`
Expected: FAIL

**Step 3: 实现 PluginManager**

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 插件清单（plugin.json）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub skills: Option<String>,
    #[serde(default)]
    pub mcp_servers: Option<String>,
    #[serde(default)]
    pub hooks: Option<String>,
}

/// 插件元数据（注册表条目）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMeta {
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub installed_at: String,
    pub source: String,
}

/// 全局注册表（registry.json）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PluginRegistry {
    plugins: HashMap<String, PluginMeta>,
}

/// 插件安装来源
#[derive(Debug, Clone)]
pub enum PluginSource {
    Local { path: PathBuf },
}

/// 插件管理器
pub struct PluginManager {
    registry: PluginRegistry,
    plugins_dir: PathBuf,
}

impl PluginManager {
    /// 加载插件注册表
    pub fn load(plugins_dir: &Path) -> Result<Self, String> {
        let registry_path = plugins_dir.join("registry.json");
        let registry = if registry_path.exists() {
            let content = std::fs::read_to_string(&registry_path)
                .map_err(|e| format!("读取 registry.json 失败: {e}"))?;
            serde_json::from_str(&content)
                .map_err(|e| format!("解析 registry.json 失败: {e}"))?
        } else {
            PluginRegistry::default()
        };

        Ok(Self {
            registry,
            plugins_dir: plugins_dir.to_path_buf(),
        })
    }

    /// 安装插件（从本地目录）
    pub fn install(&mut self, source: &Path, publisher: &str) -> Result<String, String> {
        let manifest_path = source.join("plugin.json");
        let manifest: PluginManifest = {
            let content = std::fs::read_to_string(&manifest_path)
                .map_err(|e| format!("读取 plugin.json 失败: {e}"))?;
            serde_json::from_str(&content)
                .map_err(|e| format!("解析 plugin.json 失败: {e}"))?
        };

        let version = manifest.version.clone().unwrap_or_else(|| "0.0.0".to_string());
        let target_dir = self.plugins_dir
            .join("cache")
            .join(publisher)
            .join(&manifest.name)
            .join(&version);

        // 复制文件
        copy_dir_recursive(source, &target_dir)
            .map_err(|e| format!("复制插件文件失败: {e}"))?;

        // 更新注册表
        let meta = PluginMeta {
            name: manifest.name.clone(),
            publisher: publisher.to_string(),
            version,
            installed_at: chrono_now_rfc3339(),
            source: format!("local:{}", source.display()),
        };
        self.registry.plugins.insert(manifest.name.clone(), meta);
        self.save_registry()?;

        Ok(manifest.name)
    }

    /// 卸载插件
    pub fn uninstall(&mut self, name: &str) -> Result<(), String> {
        let meta = self.registry.plugins.remove(name)
            .ok_or_else(|| format!("插件 '{name}' 未安装"))?;

        let plugin_dir = self.plugins_dir
            .join("cache")
            .join(&meta.publisher)
            .join(name);

        if plugin_dir.exists() {
            std::fs::remove_dir_all(&plugin_dir)
                .map_err(|e| format!("删除插件目录失败: {e}"))?;
        }

        self.save_registry()?;
        Ok(())
    }

    /// 列出已安装插件
    pub fn list(&self) -> Vec<&PluginMeta> {
        self.registry.plugins.values().collect()
    }

    /// 获取插件的安装目录
    pub fn plugin_dir(&self, name: &str) -> Option<PathBuf> {
        let meta = self.registry.plugins.get(name)?;
        Some(self.plugins_dir
            .join("cache")
            .join(&meta.publisher)
            .join(name)
            .join(&meta.version))
    }

    /// 获取所有插件的 skills/ 目录路径
    pub fn skill_roots(&self) -> Vec<PathBuf> {
        self.registry.plugins.values().filter_map(|meta| {
            let dir = self.plugins_dir
                .join("cache")
                .join(&meta.publisher)
                .join(&meta.name)
                .join(&meta.version)
                .join("skills");
            if dir.exists() { Some(dir) } else { None }
        }).collect()
    }

    /// 获取所有插件的 MCP 配置文件路径
    pub fn mcp_configs(&self) -> Vec<PathBuf> {
        self.registry.plugins.values().filter_map(|meta| {
            let plugin_dir = self.plugins_dir
                .join("cache")
                .join(&meta.publisher)
                .join(&meta.name)
                .join(&meta.version);
            let manifest_path = plugin_dir.join("plugin.json");
            let content = std::fs::read_to_string(&manifest_path).ok()?;
            let manifest: PluginManifest = serde_json::from_str(&content).ok()?;
            manifest.mcp_servers.map(|rel| plugin_dir.join(rel.trim_start_matches("./")))
        }).collect()
    }

    fn save_registry(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.plugins_dir)
            .map_err(|e| format!("创建插件目录失败: {e}"))?;
        let json = serde_json::to_string_pretty(&self.registry)
            .map_err(|e| format!("序列化注册表失败: {e}"))?;
        std::fs::write(self.plugins_dir.join("registry.json"), json)
            .map_err(|e| format!("写入 registry.json 失败: {e}"))?;
        Ok(())
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn chrono_now_rfc3339() -> String {
    // 使用 std 时间替代 chrono 以减少依赖
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}s", dur.as_secs())
}
```

**Step 4: 在 Cargo.toml 添加 tempfile dev-dependency（已有）**

确认 `tempfile = "3"` 在 `[dev-dependencies]` 中。

**Step 5: 运行测试确认通过**

Run: `cd rust && cargo test -p brain-plugin`
Expected: ALL PASS

**Step 6: Commit**

```bash
git add rust/crates/brain-plugin/
git commit -m "feat(plugin): 实现 PluginManager 安装/卸载/注册表"
```

---

## Phase 2: brain-mcp crate（McpClientPool）

### Task 4: 创建 brain-mcp crate 骨架

**Files:**
- Create: `rust/crates/brain-mcp/Cargo.toml`
- Create: `rust/crates/brain-mcp/src/lib.rs`

**Step 1: 创建 Cargo.toml**

```toml
[package]
name = "brain-mcp"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
brain-core = { path = "../brain-core" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["sync", "rt", "macros", "process", "io-util"] }
tracing = "0.1"
thiserror = "2"
rmcp = "1"

[dev-dependencies]
tempfile = "3"

[lints]
workspace = true
```

**Step 2: 创建 src/lib.rs 占位**

```rust
pub mod config;
pub mod client_pool;

pub use config::McpServerConfig;
pub use client_pool::McpClientPool;
```

**Step 3: 验证 rmcp 可用**

Run: `cd rust && cargo check -p brain-mcp`
Expected: 编译通过（rmcp crate 已从 crates.io 下载）

**Step 4: Commit**

```bash
git add rust/crates/brain-mcp/
git commit -m "feat(mcp): 创建 brain-mcp crate 骨架"
```

---

### Task 5: 实现 MCP 配置加载

**Files:**
- Modify: `rust/crates/brain-mcp/src/config.rs`

**Step 1: 写测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stdio_config() {
        let json = r#"{
            "command": "npx",
            "args": ["-y", "@upstreamapi/context7-mcp@latest"],
            "env": { "API_KEY": "test" }
        }"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.command, Some("npx".to_string()));
        assert_eq!(config.args.unwrap().len(), 2);
    }

    #[test]
    fn parse_sse_config() {
        let json = r#"{
            "type": "sse",
            "url": "http://localhost:8080/sse"
        }"#;
        let config: McpServerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.transport_type, Some("sse".to_string()));
        assert_eq!(config.url, Some("http://localhost:8080/sse".to_string()));
    }

    #[test]
    fn load_mcp_servers_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_path = dir.path().join("mcp-servers.json");
        let content = r#"{
            "mcpServers": {
                "context7": {
                    "command": "npx",
                    "args": ["-y", "context7"]
                }
            }
        }"#;
        std::fs::write(&config_path, content).unwrap();

        let servers = load_mcp_servers(&config_path).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].0, "context7");
    }

    #[test]
    fn load_nonexistent_returns_empty() {
        let servers = load_mcp_servers(&std::path::PathBuf::from("/tmp/does-not-exist")).unwrap();
        assert!(servers.is_empty());
    }
}
```

**Step 2: 运行测试确认失败**

Run: `cd rust && cargo test -p brain-mcp`
Expected: FAIL

**Step 3: 实现 config 模块**

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// 单个 MCP 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// stdio 模式：启动命令
    #[serde(default)]
    pub command: Option<String>,
    /// stdio 模式：命令参数
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// 环境变量
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    /// 传输类型：sse / http（无此字段则为 stdio）
    #[serde(default, rename = "type")]
    pub transport_type: Option<String>,
    /// SSE/HTTP 模式的 URL
    #[serde(default)]
    pub url: Option<String>,
}

/// mcp-servers.json 根结构
#[derive(Debug, Deserialize)]
struct McpServersFile {
    #[serde(default, rename = "mcpServers")]
    mcp_servers: HashMap<String, McpServerConfig>,
}

/// 加载 MCP 服务器配置文件
pub fn load_mcp_servers(path: &Path) -> Result<Vec<(String, McpServerConfig)>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("读取 MCP 配置失败: {e}"))?;
    let file: McpServersFile = serde_json::from_str(&content)
        .map_err(|e| format!("解析 MCP 配置失败: {e}"))?;
    Ok(file.mcp_servers.into_iter().collect())
}
```

**Step 4: 运行测试确认通过**

Run: `cd rust && cargo test -p brain-mcp`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add rust/crates/brain-mcp/
git commit -m "feat(mcp): 实现 MCP 服务器配置加载"
```

---

### Task 6: 实现 McpClientPool

**Files:**
- Modify: `rust/crates/brain-mcp/src/client_pool.rs`

**重要说明**：rmcp crate 的 API 可能与设计文档中的伪代码有差异。实现前需要查阅 rmcp 的实际 API。以下是框架性实现，具体 rmcp 调用需根据实际 API 调整。

**Step 1: 写测试（配置解析 + 工具名映射，不含真实连接）**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mcp_tool_name() {
        let (server, tool) = parse_mcp_tool_name("mcp__context7__query_docs").unwrap();
        assert_eq!(server, "context7");
        assert_eq!(tool, "query_docs");
    }

    #[test]
    fn parse_mcp_tool_name_no_prefix_fails() {
        assert!(parse_mcp_tool_name("regular_tool").is_none());
    }

    #[test]
    fn format_mcp_tool_name() {
        let name = format_mcp_tool_name("context7", "query_docs");
        assert_eq!(name, "mcp__context7__query_docs");
    }

    #[test]
    fn new_pool_is_empty() {
        let pool = McpClientPool::new();
        assert!(pool.list_tool_names().is_empty());
    }
}
```

**Step 2: 运行测试确认失败**

Run: `cd rust && cargo test -p brain-mcp`
Expected: FAIL

**Step 3: 实现 client_pool 框架**

```rust
use crate::config::McpServerConfig;
use brain_core::types::{ToolCall, ToolDescriptor, ToolExecutionResult};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// MCP 服务器连接状态
#[derive(Debug, Clone)]
pub enum ServerStatus {
    Connected,
    Disconnected,
    Error(String),
}

/// MCP 服务器条目（运行时状态）
struct McpServerEntry {
    name: String,
    tools: Vec<ToolDescriptor>,
    status: ServerStatus,
    // rmcp Client 在 connect 后填入，这里先用 Option 占位
    // 实际 rmcp 集成时替换为真实类型
}

/// MCP 客户端连接池
pub struct McpClientPool {
    servers: Arc<Mutex<HashMap<String, McpServerEntry>>>,
}

impl McpClientPool {
    pub fn new() -> Self {
        Self {
            servers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 连接所有 MCP 服务器（当前为框架，真实 rmcp 连接在后续 task 完善）
    pub async fn connect_all(
        configs: &[(String, McpServerConfig)],
    ) -> Result<Self, String> {
        let pool = Self::new();
        let mut servers = pool.servers.lock().await;

        for (name, _config) in configs {
            // TODO: 真正的 rmcp 连接
            // 当前只记录条目，不实际连接
            servers.insert(name.clone(), McpServerEntry {
                name: name.clone(),
                tools: Vec::new(),
                status: ServerStatus::Error("MCP client not yet implemented".to_string()),
            });
            tracing::warn!("MCP 服务器 '{}' 注册为 stub（rmcp 连接待实现）", name);
        }

        Ok(pool)
    }

    /// 列出所有已注册的工具名
    pub fn list_tool_names(&self) -> Vec<String> {
        // 同步版本，用于初始化
        Vec::new()
    }

    /// 列出所有已连接服务器的工具描述符
    pub async fn list_tool_definitions(&self) -> Vec<ToolDescriptor> {
        let servers = self.servers.lock().await;
        servers.values().flat_map(|s| s.tools.clone()).collect()
    }

    /// 执行 MCP 工具调用
    pub async fn execute(&self, tool_call: &ToolCall) -> ToolExecutionResult {
        let (server_name, tool_name) = match parse_mcp_tool_name(&tool_call.tool_name) {
            Some(pair) => pair,
            None => {
                return ToolExecutionResult {
                    tool_name: tool_call.tool_name.clone(),
                    output: "Invalid MCP tool name format".to_string(),
                    is_error: true,
                    duration_ms: 0,
                };
            }
        };

        let servers = self.servers.lock().await;
        match servers.get(server_name) {
            Some(entry) => {
                match &entry.status {
                    ServerStatus::Connected => {
                        // TODO: 真正的 rmcp client.call_tool()
                        ToolExecutionResult {
                            tool_name: tool_call.tool_name.clone(),
                            output: format!("MCP tool '{}' on '{}' not yet connected", tool_name, server_name),
                            is_error: true,
                            duration_ms: 0,
                        }
                    }
                    ServerStatus::Disconnected => ToolExecutionResult {
                        tool_name: tool_call.tool_name.clone(),
                        output: format!("MCP server '{server_name}' is disconnected"),
                        is_error: true,
                        duration_ms: 0,
                    },
                    ServerStatus::Error(msg) => ToolExecutionResult {
                        tool_name: tool_call.tool_name.clone(),
                        output: format!("MCP server '{server_name}' error: {msg}"),
                        is_error: true,
                        duration_ms: 0,
                    },
                }
            }
            None => ToolExecutionResult {
                tool_name: tool_call.tool_name.clone(),
                output: format!("MCP server '{server_name}' not found"),
                is_error: true,
                duration_ms: 0,
            },
        }
    }

    /// 关闭所有连接
    pub async fn shutdown(&self) -> Result<(), String> {
        let mut servers = self.servers.lock().await;
        servers.clear();
        Ok(())
    }
}

/// 解析 MCP 工具名 "mcp__{server}__{tool}" → (server, tool)
pub fn parse_mcp_tool_name(full_name: &str) -> Option<(&str, &str)> {
    let rest = full_name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

/// 格式化 MCP 工具名
pub fn format_mcp_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}
```

**Step 4: 运行测试确认通过**

Run: `cd rust && cargo test -p brain-mcp`
Expected: ALL PASS

**Step 5: Commit**

```bash
git add rust/crates/brain-mcp/
git commit -m "feat(mcp): 实现 McpClientPool 框架（stub 连接）"
```

---

## Phase 3: 集成到主脑

### Task 7: RealToolExecutor 集成 Skill + MCP 路由

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/real_tool_executor.rs`

**Step 1: 在 Cargo.toml 添加新依赖**

```toml
brain-plugin = { path = "../brain-plugin" }
brain-mcp = { path = "../brain-mcp" }
```

**Step 2: 修改 RealToolExecutor 结构体，添加 skill_catalog 和 mcp_pool 字段**

在 `real_tool_executor.rs` 中:
- 添加 `use brain_plugin::SkillCatalog;`
- 添加 `use brain_mcp::McpClientPool;`
- 在 struct 中添加:
  ```rust
  skill_catalog: Option<Arc<SkillCatalog>>,
  mcp_pool: Option<Arc<McpClientPool>>,
  ```
- 添加 builder 方法 `with_skill_catalog()` 和 `with_mcp_pool()`

**Step 3: 修改 execute() 路由逻辑**

在 `execute()` 方法的 match 分支中，在默认分支之前添加:

```rust
// Skill 工具 → SkillCatalog
"Skill" => {
    if let Some(ref catalog) = self.skill_catalog {
        let skill_name = input.get("skill")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        match catalog.resolve(skill_name) {
            Some(meta) => match catalog.load_content(meta) {
                Ok(content) => ToolExecutionResult {
                    tool_name: name,
                    output: content,
                    is_error: false,
                    duration_ms: 0,
                },
                Err(e) => ToolExecutionResult {
                    tool_name: name,
                    output: format!("加载技能失败: {e}"),
                    is_error: true,
                    duration_ms: 0,
                },
            },
            None => ToolExecutionResult {
                tool_name: name,
                output: format!("未知技能: {skill_name}"),
                is_error: true,
                duration_ms: 0,
            },
        }
    } else {
        ToolExecutionResult {
            tool_name: name,
            output: "Skill 系统未初始化".to_string(),
            is_error: true,
            duration_ms: 0,
        }
    }
}

// MCP 工具 → McpClientPool
n if n.starts_with("mcp__") => {
    if let Some(ref pool) = self.mcp_pool {
        // MCP 执行是异步的，需要 block_on 或通过 channel
        // 在 spawn_blocking 中执行
        let pool = pool.clone();
        let call = ToolCall {
            tool_name: name.clone(),
            input: input.clone(),
            validated: call.validated,
            validation_id: call.validation_id.clone(),
        };
        // 使用 tokio runtime handle 来执行异步
        let rt = tokio::runtime::Handle::current();
        rt.block_on(pool.execute(&call))
    } else {
        ToolExecutionResult {
            tool_name: name,
            output: "MCP 系统未初始化".to_string(),
            is_error: true,
            duration_ms: 0,
        }
    }
}
```

**Step 4: 验证编译**

Run: `cd rust && cargo check -p ai-brain-cli`
Expected: PASS

**Step 5: Commit**

```bash
git add rust/crates/ai-brain-cli/
git commit -m "feat(cli): RealToolExecutor 集成 Skill + MCP 路由"
```

---

### Task 8: Orchestrator 启动流程改造

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`

**Step 1: 添加 use 声明**

```rust
use brain_plugin::{SkillCatalog, PluginManager};
use brain_mcp::{McpClientPool, config::load_mcp_servers};
```

**Step 2: 修改 create_v2_main_brain() 函数**

在 `create_v2_main_brain()` 中（约第 1649 行），在创建 RealToolExecutor 之前添加:

```rust
// 1. 加载插件管理器
let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
let plugins_dir = std::path::PathBuf::from(&home).join(".ai-brain").join("plugins");
let plugin_mgr = PluginManager::load(&plugins_dir)
    .map_err(|e| { tracing::warn!("加载插件管理器失败: {e}"); e })
    .ok();

// 2. 扫描 Skill 目录
let mut skill_roots = vec![];
// 项目级
skill_roots.push(std::env::current_dir().unwrap_or_default().join(".ai-brain").join("skills"));
// 插件级
if let Some(ref mgr) = plugin_mgr {
    skill_roots.extend(mgr.skill_roots());
}
// 用户级
skill_roots.push(std::path::PathBuf::from(&home).join(".ai-brain").join("skills"));
// 兼容层
skill_roots.push(std::path::PathBuf::from(&home).join(".claude").join("skills"));
skill_roots.push(std::path::PathBuf::from(&home).join(".codex").join("skills"));

let skill_catalog = SkillCatalog::scan_all(&skill_roots)
    .unwrap_or_else(|e| { tracing::warn!("扫描技能失败: {e}"); SkillCatalog { skills: vec![] } });

tracing::info!("扫描到 {} 个技能", skill_catalog.skills.len());

// 3. 加载 MCP 配置
let mcp_config_path = std::path::PathBuf::from(&home).join(".ai-brain").join("mcp").join("mcp-servers.json");
let mut mcp_configs = load_mcp_servers(&mcp_config_path).unwrap_or_default();
// 合并插件内的 MCP 配置
if let Some(ref mgr) = plugin_mgr {
    for path in mgr.mcp_configs() {
        mcp_configs.extend(load_mcp_servers(&path).unwrap_or_default());
    }
}
```

**Step 3: 修改 RealToolExecutor 创建**

将:
```rust
let tool_executor: Arc<dyn ToolExecutor> = Arc::new(
    RealToolExecutor::with_dispatch(memory_brain, dispatch),
);
```

改为:
```rust
let mut tool_executor = RealToolExecutor::with_dispatch(memory_brain, dispatch);
let skill_catalog_arc = Arc::new(skill_catalog);
tool_executor.set_skill_catalog(skill_catalog_arc.clone());
// MCP 连接（异步，但 create_v2_main_brain 是同步函数，需要处理）
// TODO: MCP 异步连接在 async 初始化中完成，当前先注册 stub
let mcp_pool = Arc::new(McpClientPool::new());
tool_executor.set_mcp_pool(mcp_pool);
let tool_executor: Arc<dyn ToolExecutor> = Arc::new(tool_executor);
```

**Step 4: 修改工具注册**

在 `brain.register_tools(tool_defs)` 之前，追加 MCP 工具:
```rust
// 注册 MCP 工具（当前为空，真实连接后才有）
let mcp_tool_defs: Vec<_> = vec![]; // 后续从 mcp_pool.list_tool_definitions() 获取
tool_defs.extend(mcp_tool_defs);
```

**Step 5: 修改评估脑 skill 加载**

将:
```rust
if let Some(ref mut eb) = eval_brain {
    let skills_dir = std::path::Path::new("rust/crates/brain-eval/skills");
    if let Err(e) = eb.load_skills_from_dir(skills_dir) {
        tracing::warn!("加载评估脑 skills 失败: {e}");
    }
}
```

改为:
```rust
if let Some(ref mut eb) = eval_brain {
    // 优先使用统一 SkillCatalog
    eb.set_skill_catalog(skill_catalog_arc.clone());
    // 兼容：同时加载评估脑内置 skills
    let skills_dir = std::path::Path::new("rust/crates/brain-eval/skills");
    if let Err(e) = eb.load_skills_from_dir(skills_dir) {
        tracing::warn!("加载评估脑内置 skills 失败: {e}");
    }
}
```

**Step 6: 验证编译**

Run: `cd rust && cargo check -p ai-brain-cli`
Expected: PASS

**Step 7: Commit**

```bash
git add rust/crates/ai-brain-cli/
git commit -m "feat(cli): 编排器集成 PluginManager + SkillCatalog + McpClientPool"
```

---

### Task 9: EvalBrain 集成 SkillCatalog

**Files:**
- Modify: `rust/crates/brain-eval/Cargo.toml` — 添加 brain-plugin 依赖
- Modify: `rust/crates/brain-eval/src/eval_brain.rs` — 添加 set_skill_catalog 方法

**Step 1: 在 brain-eval/Cargo.toml 添加依赖**

```toml
brain-plugin = { path = "../brain-plugin" }
```

**Step 2: 在 EvalBrain 添加 skill_catalog 字段和方法**

```rust
use std::sync::Arc;
use brain_plugin::SkillCatalog;

pub struct EvalBrain {
    llm: Arc<dyn LlmProvider>,
    tool_executor: Option<Arc<dyn ToolExecutor>>,
    skill_registry: SkillRegistry,        // 保留，兼容内置 skills
    skill_catalog: Option<Arc<SkillCatalog>>, // 新增：统一目录
    progress_tx: Option<tokio::sync::mpsc::Sender<ProgressEvent>>,
}

impl EvalBrain {
    pub fn set_skill_catalog(&mut self, catalog: Arc<SkillCatalog>) {
        self.skill_catalog = Some(catalog);
    }
}
```

**Step 3: 修改 eval_tool_loop 中 Skill 处理逻辑**

在 Skill tool 特殊处理中，优先查 skill_catalog：

```rust
if name == "Skill" {
    let skill_name = input.get("command")
        .or_else(|| input.get("skill"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // 优先查统一目录
    let content = if let Some(ref catalog) = external_catalog {
        match catalog.resolve(skill_name) {
            Some(meta) => catalog.load_content(meta)
                .unwrap_or_else(|_| format!("Skill '{skill_name}' 加载失败")),
            None => {
                // 回退到内置 registry
                skill_registry.get_skill_content(skill_name)
                    .map_or_else(|| format!("Skill '{skill_name}' 不存在"), ToString::to_string)
            }
        }
    } else {
        skill_registry.get_skill_content(skill_name)
            .map_or_else(|| format!("Skill '{skill_name}' 不存在"), ToString::to_string)
    };

    messages.push(ChatMessage::tool_result(&id, content, false));
    continue;
}
```

**Step 4: 验证编译**

Run: `cd rust && cargo check -p brain-eval`
Expected: PASS

**Step 5: Commit**

```bash
git add rust/crates/brain-eval/
git commit -m "feat(eval): 评估脑集成统一 SkillCatalog"
```

---

## Phase 4: 清理 + CLI 命令

### Task 10: 清理旧 Skill 代码

**Files:**
- Modify: `rust/crates/tools/src/lib.rs` — 删除 resolve_skill_path() + parse_skill_description() + execute_skill()

**Step 1: 删除 tools/src/lib.rs 中以下函数**

- `resolve_skill_path()` (约第 1849 行)
- `parse_skill_description()` (约第 3921 行)
- `execute_skill()` (约第 1813 行)
- `SkillInput` 和 `SkillOutput` 结构体

**Step 2: 修改 execute_tool() 中的 "Skill" 分支**

将 "Skill" 分支从 tools crate 中移除（现在由 RealToolExecutor 直接处理）。
但保留 Skill 的 ToolSpec 定义（用于注册到 LLM），只删除执行逻辑。

**Step 3: 验证编译 + 测试**

Run: `cd rust && cargo test --workspace`
Expected: ALL PASS（Skill 工具不再由 tools crate 执行）

**Step 4: Commit**

```bash
git add rust/crates/tools/
git commit -m "refactor(tools): 移除旧 Skill 执行逻辑（已迁移到 brain-plugin）"
```

---

### Task 11: CLI 命令 — /plugin 和 /mcp

**Files:**
- Modify: `rust/crates/commands/src/lib.rs` — 添加 /plugin 和 /mcp 命令处理
- Modify: `rust/crates/ai-brain-cli/src/tui/app.rs` — 添加路由

**Step 1: 在 commands/src/lib.rs 添加命令处理**

```rust
pub fn handle_plugin_command(args: Option<&str>) -> std::io::Result<String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let plugins_dir = std::path::PathBuf::from(&home).join(".ai-brain").join("plugins");

    match args.map(|a| a.trim()).unwrap_or("") {
        "list" | "" => {
            let mgr = brain_plugin::PluginManager::load(&plugins_dir)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            let plugins = mgr.list();
            if plugins.is_empty() {
                return Ok("没有已安装的插件。".to_string());
            }
            let mut out = String::from("已安装插件:\n");
            for p in plugins {
                out.push_str(&format!("  {} v{} ({})\n", p.name, p.version, p.publisher));
            }
            Ok(out)
        }
        install_args if install_args.starts_with("install ") => {
            let path = install_args["install ".len()..].trim();
            let mut mgr = brain_plugin::PluginManager::load(&plugins_dir)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            let name = mgr.install(std::path::Path::new(path), "local")
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            Ok(format!("插件 '{name}' 安装成功。"))
        }
        uninstall_args if uninstall_args.starts_with("uninstall ") => {
            let name = uninstall_args["uninstall ".len()..].trim();
            let mut mgr = brain_plugin::PluginManager::load(&plugins_dir)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            mgr.uninstall(name)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            Ok(format!("插件 '{name}' 已卸载。"))
        }
        _ => Ok("用法: /plugin list | install <path> | uninstall <name>".to_string()),
    }
}
```

**Step 2: 验证编译**

Run: `cd rust && cargo check -p commands`
Expected: PASS

**Step 3: Commit**

```bash
git add rust/crates/commands/
git commit -m "feat(cli): 添加 /plugin 和 /mcp CLI 命令"
```

---

### Task 12: 全量测试 + 更新 MEMORY

**Step 1: 运行全量测试**

Run: `cd rust && cargo test --workspace`
Expected: ALL PASS

**Step 2: 运行 clippy**

Run: `cd rust && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS（修复所有 warning）

**Step 3: 更新项目记忆**

更新 `/Users/chenh/.claude/projects/-Users-chenh-RustObject-claw-code-parity/memory/MEMORY.md`:
- 添加 brain-plugin 和 brain-mcp crate 信息
- 更新 crate 结构
- 记录新的插件系统设计

**Step 4: 最终 Commit**

```bash
git add -A
git commit -m "feat: 智脑原生 Skill/Plugin/MCP 系统完成

新增 crate:
- brain-plugin: SkillLoader + PluginManager
- brain-mcp: McpClientPool（rmcp stub）

集成:
- RealToolExecutor 路由 Skill + MCP 工具
- Orchestrator 启动时扫描插件和技能
- EvalBrain 使用统一 SkillCatalog
- CLI: /plugin install/uninstall/list

目录: ~/.ai-brain/
命名空间: plugin-name:skill-name
"
```

---

## 依赖关系

```
Task 1 (骨架) ──→ Task 2 (SkillLoader) ──→ Task 3 (PluginManager) ──→ Task 7 (RealToolExecutor)
                       │                                                │
                       └────────────────────────────────────────────────┴──→ Task 8 (Orchestrator)
                                                                                │
Task 4 (骨架) ──→ Task 5 (MCP config) ──→ Task 6 (McpClientPool) ─────────────┘
                                                       │
                                                       └──→ Task 9 (EvalBrain)

Task 7-9 完成 ──→ Task 10 (清理) ──→ Task 11 (CLI) ──→ Task 12 (全量测试)
```

## 后续迭代（不在本计划范围）

- rmcp 真实连接实现（替换 Task 6 的 stub）
- Git 安装来源支持
- MCP 健康检查 + 自动重连
- LSP/Monitors 组件
- Hooks 事件系统集成
- 插件版本升级机制
