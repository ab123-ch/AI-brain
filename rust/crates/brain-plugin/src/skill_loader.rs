use std::path::PathBuf;

/// Skill 元数据（从 SKILL.md frontmatter 解析）
#[derive(Debug, Clone)]
pub struct SkillMeta {
    pub name: String,
    pub namespace: Option<String>,
    pub description: String,
    pub when_to_use: Option<String>,
    pub source_path: PathBuf,
}

/// Skill 目录
#[derive(Debug, Clone)]
pub struct SkillCatalog {
    pub skills: Vec<SkillMeta>,
}
