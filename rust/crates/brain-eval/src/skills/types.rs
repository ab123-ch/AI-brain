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
    fn parse_skill_content_extracts_name_and_description() {
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