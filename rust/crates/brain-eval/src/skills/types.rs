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