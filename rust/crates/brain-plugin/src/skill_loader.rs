use std::fmt::Write;
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
    /// 无冒号时：先查无 namespace 的，再查有 namespace 的
    pub fn resolve(&self, query: &str) -> Option<&SkillMeta> {
        if let Some((ns, name)) = query.split_once(':') {
            self.skills
                .iter()
                .find(|s| s.namespace.as_deref() == Some(ns) && s.name == name)
        } else {
            self.skills
                .iter()
                .find(|s| s.namespace.is_none() && s.name == query)
                .or_else(|| self.skills.iter().find(|s| s.name == query))
        }
    }

    /// 读取 SKILL.md 的 body（不含 frontmatter）
    pub fn load_content(&self, meta: &SkillMeta) -> Result<String, String> {
        let content = std::fs::read_to_string(&meta.source_path)
            .map_err(|e| format!("读取 SKILL.md 失败: {e}"))?;
        let parsed =
            parse_skill_content(&content).ok_or_else(|| "解析 SKILL.md 失败".to_string())?;
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
    components.next(); // SKILL.md
    components.next(); // {name}
    components.next(); // skills
    let plugin = components.next()?.as_os_str().to_string_lossy().to_string();
    components.next(); // {version}
    let maybe_cache = components.next();
    if let Some(c) = maybe_cache {
        if c.as_os_str() == "cache" {
            return Some(plugin);
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
    let end_pos = rest
        .find("\n---")
        .or_else(|| rest.find("\r\n---"))?;

    let frontmatter = &rest[..end_pos];
    let body_start = end_pos + 3;
    let body = rest[body_start..]
        .trim_start_matches(|c: char| c == '\n' || c == '\r')
        .trim()
        .to_string();

    let name = extract_field(frontmatter, "name")?;
    let description = extract_field(frontmatter, "description")?;
    let when_to_use = extract_field(frontmatter, "when_to_use");

    Some(ParsedSkill {
        name,
        description,
        when_to_use,
        body,
    })
}

/// 从 YAML frontmatter 提取字段值（支持单引号和双引号）
fn extract_field(yaml: &str, field: &str) -> Option<String> {
    let prefix = format!("{field}:");
    for line in yaml.lines() {
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix(&prefix) {
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| {
                    value
                        .strip_prefix('\'')
                        .and_then(|v| v.strip_suffix('\''))
                })
                .unwrap_or(value);
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

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
        assert_eq!(
            result.description,
            "Force brainstorming before implementation"
        );
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
        let content = "---\nname: tdd\ndescription: \"TDD skill\"\nwhen_to_use: \"when writing code\"\n---\n# TDD";
        let result = parse_skill_content(&content).unwrap();
        assert_eq!(result.name, "tdd");
        assert_eq!(
            result.when_to_use,
            Some("when writing code".to_string())
        );
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
        )
        .unwrap();

        let catalog =
            SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(catalog.skills[0].name, "brainstorming");
    }

    #[test]
    fn scan_empty_dir_ok() {
        let dir = TempDir::new().unwrap();
        let catalog =
            SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        assert!(catalog.skills.is_empty());
    }

    #[test]
    fn scan_nonexistent_dir_ok() {
        let catalog =
            SkillCatalog::scan_all(&[PathBuf::from("/tmp/does-not-exist-xyz")])
                .unwrap();
        assert!(catalog.skills.is_empty());
    }

    #[test]
    fn resolve_by_name() {
        let catalog = SkillCatalog {
            skills: vec![SkillMeta {
                name: "brainstorming".into(),
                namespace: None,
                description: "desc".into(),
                when_to_use: None,
                source_path: PathBuf::from("/fake/SKILL.md"),
            }],
        };
        assert!(catalog.resolve("brainstorming").is_some());
        assert!(catalog.resolve("nonexistent").is_none());
    }

    #[test]
    fn resolve_with_namespace() {
        let catalog = SkillCatalog {
            skills: vec![SkillMeta {
                name: "brainstorming".into(),
                namespace: Some("superpowers".into()),
                description: "desc".into(),
                when_to_use: None,
                source_path: PathBuf::from("/fake/SKILL.md"),
            }],
        };
        assert!(catalog.resolve("superpowers:brainstorming").is_some());
        assert!(catalog.resolve("brainstorming").is_some());
    }

    #[test]
    fn summary_for_prompt_xml() {
        let catalog = SkillCatalog {
            skills: vec![SkillMeta {
                name: "brainstorming".into(),
                namespace: None,
                description: "Force brainstorming".into(),
                when_to_use: None,
                source_path: PathBuf::from("/fake"),
            }],
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
        )
        .unwrap();

        let catalog =
            SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        let content = catalog.load_content(&catalog.skills[0]).unwrap();
        assert!(content.contains("# TDD Rules"));
        assert!(!content.contains("name: tdd"));
    }
}
