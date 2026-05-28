use std::collections::HashSet;
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
    /// 所属技能包名（None 表示独立技能）
    pub pack_name: Option<String>,
    /// 是否为启动时自动注入的 bootstrap 技能
    pub is_bootstrap: bool,
}

/// 技能包元数据
#[derive(Debug, Clone)]
pub struct SkillPackMeta {
    /// 技能包名称，如 "superpowers"
    pub name: String,
    /// 描述（从 manifest 读取）
    pub description: Option<String>,
    /// 版本
    pub version: Option<String>,
    /// 技能包根目录
    pub root_path: PathBuf,
    /// manifest 文件路径
    pub manifest_path: Option<PathBuf>,
    /// 子技能目录路径
    pub skills_dir: PathBuf,
    /// bootstrap 技能名（启动时自动注入）
    pub bootstrap_skill: Option<String>,
}

/// Skill 解析结果（内部用）
struct ParsedSkill {
    name: String,
    description: String,
    when_to_use: Option<String>,
    bootstrap: bool,
    body: String,
}

/// Manifest 探测白名单路径
const MANIFEST_PATHS: &[&str] = &[
    ".claude-plugin/plugin.json",
    ".cursor-plugin/plugin.json",
    ".codex-plugin/plugin.json",
    "gemini-extension.json",
];

/// Skill 目录（启动时扫描，运行时查询）
#[derive(Debug, Clone)]
pub struct SkillCatalog {
    /// 所有已注册的技能
    pub skills: Vec<SkillMeta>,
    /// 已识别的技能包
    pub packs: Vec<SkillPackMeta>,
    /// 启动时自动注入的 bootstrap 技能
    pub bootstrap_skills: Vec<SkillMeta>,
}

impl SkillCatalog {
    /// 扫描多个根目录，收集所有 SKILL.md 的元数据
    ///
    /// 两阶段扫描：
    /// - A 类：直接技能（目录中有 SKILL.md）
    /// - B 类：结构化技能包（有 manifest + skills/ 子目录）
    /// - C 类：展平技能包（无 SKILL.md 无 manifest，但子目录中有多个 SKILL.md）
    pub fn scan_all(roots: &[PathBuf]) -> Result<Self, String> {
        let mut skills = Vec::new();
        let mut packs = Vec::new();
        let mut bootstrap_skills = Vec::new();
        let mut seen_paths: HashSet<PathBuf> = HashSet::new();

        for root in roots {
            if !root.exists() {
                continue;
            }
            if let Ok(entries) = std::fs::read_dir(root) {
                for entry in entries.flatten() {
                    let dir = entry.path();
                    if !dir.is_dir() {
                        continue;
                    }

                    // 优先级 A：直接技能
                    let skill_file = dir.join("SKILL.md");
                    if skill_file.exists() {
                        if let Some(parsed) = parse_skill_file(&skill_file) {
                            if seen_paths.insert(skill_file.clone()) {
                                let namespace = infer_namespace(&skill_file);
                                let meta = SkillMeta {
                                    name: parsed.name,
                                    namespace,
                                    description: parsed.description,
                                    when_to_use: parsed.when_to_use,
                                    source_path: skill_file,
                                    pack_name: None,
                                    is_bootstrap: false,
                                };
                                skills.push(meta);
                            }
                        }
                        continue;
                    }

                    // 优先级 B：结构化技能包（有 manifest）
                    if let Some(pack) = detect_structured_pack(&dir) {
                        let pack_name = pack.name.clone();
                        let bootstrap_name = detect_bootstrap_skill(&pack.skills_dir, &pack_name);
                        let sub_skills =
                            scan_skills_in_dir(&pack.skills_dir, &pack_name, &pack_name);
                        for meta in sub_skills {
                            if seen_paths.insert(meta.source_path.clone()) {
                                let is_bootstrap = bootstrap_name.as_deref() == Some(&meta.name);
                                if is_bootstrap {
                                    bootstrap_skills.push(meta.clone());
                                }
                                skills.push(meta);
                            }
                        }
                        packs.push(pack);
                        continue;
                    }

                    // 优先级 C：展平技能包
                    if let Some((pack, sub_skills)) = try_scan_as_flat_pack(&dir) {
                        let pack_name = pack.name.clone();
                        let bootstrap_name =
                            detect_bootstrap_skill(&pack.skills_dir, &pack_name);
                        for mut meta in sub_skills {
                            if seen_paths.insert(meta.source_path.clone()) {
                                let is_bootstrap = bootstrap_name.as_deref() == Some(&meta.name);
                                meta.is_bootstrap = is_bootstrap;
                                if is_bootstrap {
                                    bootstrap_skills.push(meta.clone());
                                }
                                skills.push(meta);
                            }
                        }
                        packs.push(pack);
                    }
                }
            }
        }

        tracing::info!(
            "扫描完成: {} 个技能, {} 个技能包, {} 个 bootstrap 技能",
            skills.len(),
            packs.len(),
            bootstrap_skills.len()
        );

        Ok(Self {
            skills,
            packs,
            bootstrap_skills,
        })
    }

    /// 解析查询：支持 4 层匹配
    ///
    /// 1. 命名空间限定: "superpowers:brainstorming" → 精确匹配
    /// 2. 精确名称: "brainstorming" → 先无 namespace，再 fallback
    /// 3. 包名匹配: "superpowers" → 返回 bootstrap 或第一个子技能
    /// 4. 模糊匹配: "brain" → name 或 description 包含关键词
    pub fn resolve(&self, query: &str) -> Option<&SkillMeta> {
        // 1. 命名空间限定
        if let Some((ns, name)) = query.split_once(':') {
            return self
                .skills
                .iter()
                .find(|s| s.namespace.as_deref() == Some(ns) && s.name == name);
        }

        // 2. 精确名称
        if let Some(exact) = self
            .skills
            .iter()
            .find(|s| s.namespace.is_none() && s.name == query)
        {
            return Some(exact);
        }
        if let Some(fallback) = self.skills.iter().find(|s| s.name == query) {
            return Some(fallback);
        }

        // 3. 包名匹配：返回 bootstrap 技能或第一个子技能
        if let Some(pack) = self.packs.iter().find(|p| p.name == query) {
            if let Some(ref bs_name) = pack.bootstrap_skill {
                if let Some(bs) = self.skills.iter().find(|s| {
                    s.pack_name.as_deref() == Some(&pack.name) && s.name == *bs_name
                }) {
                    return Some(bs);
                }
            }
            // fallback 到包下第一个技能
            return self
                .skills
                .iter()
                .find(|s| s.pack_name.as_deref() == Some(&pack.name));
        }

        // 4. 模糊匹配
        self.skills
            .iter()
            .find(|s| {
                s.name.contains(query) || s.description.to_lowercase().contains(&query.to_lowercase())
            })
    }

    /// 列出包下所有技能
    pub fn resolve_pack(&self, pack_name: &str) -> Vec<&SkillMeta> {
        self.skills
            .iter()
            .filter(|s| s.pack_name.as_deref() == Some(pack_name))
            .collect()
    }

    /// 查询包元数据
    pub fn resolve_pack_meta(&self, pack_name: &str) -> Option<&SkillPackMeta> {
        self.packs.iter().find(|p| p.name == pack_name)
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
        if self.skills.is_empty() && self.packs.is_empty() {
            return String::new();
        }
        let mut s = String::from("\n<available_skills>\n");

        // 技能包信息段
        if !self.packs.is_empty() {
            s.push_str("  <skill_packs>\n");
            for pack in &self.packs {
                let _ = writeln!(s, "    <pack name=\"{}\">", pack.name);
                if let Some(ref desc) = pack.description {
                    let _ = writeln!(s, "      <description>{desc}</description>");
                }
                let count = self
                    .skills
                    .iter()
                    .filter(|sk| sk.pack_name.as_deref() == Some(&pack.name))
                    .count();
                let _ = writeln!(s, "      <skill_count>{count}</skill_count>");
                let _ = writeln!(s, "    </pack>");
            }
            s.push_str("  </skill_packs>\n");
        }

        // 技能列表
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
    let rest = after_first.trim_start_matches(['\n', '\r']);
    let end_pos = rest.find("\n---").or_else(|| rest.find("\r\n---"))?;

    let frontmatter = &rest[..end_pos];
    let body_start = end_pos + 3;
    let body = rest[body_start..]
        .trim_start_matches(['\n', '\r'])
        .trim()
        .to_string();

    let name = extract_field(frontmatter, "name")?;
    let description = extract_field(frontmatter, "description")?;
    let when_to_use = extract_field(frontmatter, "when_to_use");
    let bootstrap = extract_field(frontmatter, "bootstrap").as_deref() == Some("true");

    Some(ParsedSkill {
        name,
        description,
        when_to_use,
        bootstrap,
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
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// 探测 manifest 文件，返回 (path, name, description, version)
fn find_manifest(dir: &Path) -> Option<(PathBuf, String, Option<String>, Option<String>)> {
    for rel in MANIFEST_PATHS {
        let manifest_path = dir.join(rel);
        if manifest_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&manifest_path) {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                    let name = json.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    // 校验：name 非空且不含路径分隔符
                    if name.is_empty() || name.contains('/') || name.contains('\\') {
                        continue;
                    }
                    let description = json
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let version = json
                        .get("version")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    return Some((manifest_path, name.to_string(), description, version));
                }
            }
        }
    }
    None
}

/// B 类：探测结构化技能包（有 manifest + skills/ 子目录）
fn detect_structured_pack(dir: &Path) -> Option<SkillPackMeta> {
    let (manifest_path, name, description, version) = find_manifest(dir)?;
    let skills_dir = dir.join("skills");
    if !skills_dir.is_dir() {
        return None;
    }
    Some(SkillPackMeta {
        name,
        description,
        version,
        root_path: dir.to_path_buf(),
        manifest_path: Some(manifest_path),
        skills_dir,
        bootstrap_skill: None,
    })
}

/// C 类：探测展平技能包（无 SKILL.md、无 manifest，但子目录中 >=2 个有 SKILL.md）
fn try_scan_as_flat_pack(dir: &Path) -> Option<(SkillPackMeta, Vec<SkillMeta>)> {
    let dir_name = dir.file_name()?.to_string_lossy().to_string();
    let mut sub_skills = Vec::new();
    let mut skill_count = 0u32;

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let sub = entry.path();
            if !sub.is_dir() {
                continue;
            }
            let sf = sub.join("SKILL.md");
            if sf.exists() {
                if let Some(parsed) = parse_skill_file(&sf) {
                    let namespace = infer_namespace(&sf)
                        .unwrap_or_else(|| dir_name.clone());
                    sub_skills.push(SkillMeta {
                        name: parsed.name,
                        namespace: Some(namespace.clone()),
                        description: parsed.description,
                        when_to_use: parsed.when_to_use,
                        source_path: sf,
                        pack_name: Some(dir_name.clone()),
                        is_bootstrap: false,
                    });
                    skill_count += 1;
                }
            }
        }
    }

    // 至少 2 个有效 SKILL.md（1 个可能是巧合）
    if skill_count < 2 {
        return None;
    }

    let pack = SkillPackMeta {
        name: dir_name,
        description: None,
        version: None,
        root_path: dir.to_path_buf(),
        manifest_path: None,
        skills_dir: dir.to_path_buf(),
        bootstrap_skill: None,
    };

    Some((pack, sub_skills))
}

/// 扫描指定目录下的子技能
fn scan_skills_in_dir(dir: &Path, namespace: &str, pack_name: &str) -> Vec<SkillMeta> {
    let mut result = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let sub = entry.path();
            if !sub.is_dir() {
                continue;
            }
            let sf = sub.join("SKILL.md");
            if sf.exists() {
                if let Some(parsed) = parse_skill_file(&sf) {
                    result.push(SkillMeta {
                        name: parsed.name,
                        namespace: Some(namespace.to_string()),
                        description: parsed.description,
                        when_to_use: parsed.when_to_use,
                        source_path: sf,
                        pack_name: Some(pack_name.to_string()),
                        is_bootstrap: parsed.bootstrap,
                    });
                }
            }
        }
    }
    result
}

/// 探测 bootstrap 技能
///
/// 发现规则（优先级从高到低）：
/// 1. 子技能 frontmatter 含 `bootstrap: true`
/// 2. 约定式：包下存在 `skills/using-{pack_name}/SKILL.md`
fn detect_bootstrap_skill(skills_dir: &Path, pack_name: &str) -> Option<String> {
    // 规则 1: 检查含 bootstrap: true 的子技能
    if let Ok(entries) = std::fs::read_dir(skills_dir) {
        for entry in entries.flatten() {
            let sub = entry.path();
            if !sub.is_dir() {
                continue;
            }
            let sf = sub.join("SKILL.md");
            if sf.exists() {
                if let Some(parsed) = parse_skill_file(&sf) {
                    if parsed.bootstrap {
                        return Some(parsed.name);
                    }
                }
            }
        }
    }

    // 规则 2: 约定式 using-{pack_name}
    let convention_dir = skills_dir.join(format!("using-{pack_name}"));
    let convention_file = convention_dir.join("SKILL.md");
    if convention_file.exists() {
        if let Some(parsed) = parse_skill_file(&convention_file) {
            return Some(parsed.name);
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
        format!("---\nname: {name}\ndescription: \"{description}\"\n---\n\n{body}")
    }

    fn make_skill_md_with_bootstrap(name: &str, description: &str, bootstrap: bool, body: &str) -> String {
        format!(
            "---\nname: {name}\ndescription: \"{description}\"\nbootstrap: {bootstrap}\n---\n\n{body}"
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
        assert_eq!(result.when_to_use, Some("when writing code".to_string()));
    }

    #[test]
    fn parse_skill_quoted_values() {
        let content = "---\nname: 'my-skill'\ndescription: \"A skill\"\n---\nbody";
        let result = parse_skill_content(&content).unwrap();
        assert_eq!(result.name, "my-skill");
    }

    #[test]
    fn parse_skill_bootstrap_field() {
        let content = make_skill_md_with_bootstrap("using-superpowers", "Bootstrap skill", true, "# Bootstrap");
        let result = parse_skill_content(&content).unwrap();
        assert_eq!(result.name, "using-superpowers");
        assert!(result.bootstrap);
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

        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(catalog.skills[0].name, "brainstorming");
        assert!(catalog.skills[0].pack_name.is_none());
        assert!(!catalog.skills[0].is_bootstrap);
    }

    #[test]
    fn scan_empty_dir_ok() {
        let dir = TempDir::new().unwrap();
        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        assert!(catalog.skills.is_empty());
        assert!(catalog.packs.is_empty());
        assert!(catalog.bootstrap_skills.is_empty());
    }

    #[test]
    fn scan_nonexistent_dir_ok() {
        let catalog = SkillCatalog::scan_all(&[PathBuf::from("/tmp/does-not-exist-xyz")]).unwrap();
        assert!(catalog.skills.is_empty());
    }

    // --- A 类：直接技能 ---

    #[test]
    fn scan_direct_skill() {
        let dir = TempDir::new().unwrap();
        let skill_dir = dir.path().join("my-skill");
        fs::create_dir(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            make_skill_md("my-skill", "A standalone skill", "# Content"),
        )
        .unwrap();

        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(catalog.skills.len(), 1);
        assert!(catalog.packs.is_empty());
        assert_eq!(catalog.skills[0].name, "my-skill");
    }

    // --- B 类：结构化技能包 ---

    #[test]
    fn scan_structured_pack() {
        let dir = TempDir::new().unwrap();
        let pack_dir = dir.path().join("superpowers");
        let skills_dir = pack_dir.join("skills");
        let plugin_dir = pack_dir.join(".claude-plugin");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::create_dir_all(skills_dir.join("brainstorming")).unwrap();
        fs::create_dir_all(skills_dir.join("tdd")).unwrap();

        // manifest
        fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"name": "superpowers", "description": "Core skills", "version": "5.1.0"}"#,
        )
        .unwrap();

        // 子技能
        fs::write(
            skills_dir.join("brainstorming").join("SKILL.md"),
            make_skill_md("brainstorming", "Brainstorm before coding", "# Brain"),
        )
        .unwrap();
        fs::write(
            skills_dir.join("tdd").join("SKILL.md"),
            make_skill_md("tdd", "Test driven dev", "# TDD"),
        )
        .unwrap();

        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(catalog.packs.len(), 1);
        assert_eq!(catalog.packs[0].name, "superpowers");
        assert_eq!(catalog.packs[0].version.as_deref(), Some("5.1.0"));
        assert_eq!(catalog.skills.len(), 2);
        // 所有子技能应该有 pack_name
        for skill in &catalog.skills {
            assert_eq!(skill.pack_name.as_deref(), Some("superpowers"));
        }
    }

    // --- C 类：展平技能包 ---

    #[test]
    fn scan_flat_pack() {
        let dir = TempDir::new().unwrap();
        let pack_dir = dir.path().join("my-pack");
        fs::create_dir_all(pack_dir.join("skill-a")).unwrap();
        fs::create_dir_all(pack_dir.join("skill-b")).unwrap();

        fs::write(
            pack_dir.join("skill-a").join("SKILL.md"),
            make_skill_md("skill-a", "Skill A", "# A"),
        )
        .unwrap();
        fs::write(
            pack_dir.join("skill-b").join("SKILL.md"),
            make_skill_md("skill-b", "Skill B", "# B"),
        )
        .unwrap();

        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(catalog.packs.len(), 1);
        assert_eq!(catalog.packs[0].name, "my-pack");
        assert_eq!(catalog.skills.len(), 2);
    }

    #[test]
    fn scan_flat_pack_single_skill_ignored() {
        let dir = TempDir::new().unwrap();
        let pack_dir = dir.path().join("lonely-pack");
        fs::create_dir_all(pack_dir.join("only-skill")).unwrap();
        fs::write(
            pack_dir.join("only-skill").join("SKILL.md"),
            make_skill_md("only-skill", "Only one", "# Content"),
        )
        .unwrap();

        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        // 单个技能不满足 C 类条件（需 >=2），也不满足 A 类（pack_dir 没有 SKILL.md）
        assert!(catalog.packs.is_empty());
        assert!(catalog.skills.is_empty());
    }

    #[test]
    fn scan_empty_dir_skipped() {
        let dir = TempDir::new().unwrap();
        let empty = dir.path().join("empty-dir");
        fs::create_dir(&empty).unwrap();

        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        assert!(catalog.skills.is_empty());
        assert!(catalog.packs.is_empty());
    }

    #[test]
    fn scan_no_duplicate_paths() {
        let dir = TempDir::new().unwrap();
        let skill_dir = dir.path().join("dup");
        fs::create_dir(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            make_skill_md("dup", "desc", "# Body"),
        )
        .unwrap();

        // 同一个根目录扫两次也不应重复
        let catalog =
            SkillCatalog::scan_all(&[dir.path().to_path_buf(), dir.path().to_path_buf()])
                .unwrap();
        assert_eq!(catalog.skills.len(), 1);
    }

    // --- Manifest 测试 ---

    #[test]
    fn manifest_variants() {
        for rel in MANIFEST_PATHS {
            let dir = TempDir::new().unwrap();
            let pack_dir = dir.path().join("test-pack");
            let skills_dir = pack_dir.join("skills");
            let manifest_dir = pack_dir.join(Path::new(rel).parent().unwrap());
            fs::create_dir_all(&manifest_dir).unwrap();
            fs::create_dir_all(skills_dir.join("s1")).unwrap();

            fs::write(
                pack_dir.join(rel),
                r#"{"name": "test-pack"}"#,
            )
            .unwrap();
            fs::write(
                skills_dir.join("s1").join("SKILL.md"),
                make_skill_md("s1", "Skill 1", "# S1"),
            )
            .unwrap();

            let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
            assert_eq!(catalog.packs.len(), 1, "Failed for manifest path: {rel}");
            assert_eq!(catalog.packs[0].name, "test-pack");
        }
    }

    #[test]
    fn manifest_validation_rejects_bad() {
        let dir = TempDir::new().unwrap();
        let pack_dir = dir.path().join("bad-pack");
        let plugin_dir = pack_dir.join(".claude-plugin");
        let skills_dir = pack_dir.join("skills");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::create_dir_all(skills_dir.join("s1")).unwrap();

        // name 含 /
        fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"name": "bad/name"}"#,
        )
        .unwrap();
        fs::write(
            skills_dir.join("s1").join("SKILL.md"),
            make_skill_md("s1", "Skill 1", "# S1"),
        )
        .unwrap();

        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        // manifest 校验失败，不应识别为 B 类
        // 但如果 skills_dir 下只有一个子技能，也不满足 C 类
        assert!(catalog.packs.is_empty());
        assert!(catalog.skills.is_empty());
    }

    // --- Bootstrap 测试 ---

    #[test]
    fn bootstrap_frontmatter() {
        let dir = TempDir::new().unwrap();
        let pack_dir = dir.path().join("pack");
        let plugin_dir = pack_dir.join(".claude-plugin");
        let skills_dir = pack_dir.join("skills");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::create_dir_all(skills_dir.join("bootstrap-skill")).unwrap();
        fs::create_dir_all(skills_dir.join("normal-skill")).unwrap();

        fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"name": "pack"}"#,
        )
        .unwrap();
        fs::write(
            skills_dir.join("bootstrap-skill").join("SKILL.md"),
            make_skill_md_with_bootstrap("bootstrap-skill", "Bootstrap!", true, "# BS"),
        )
        .unwrap();
        fs::write(
            skills_dir.join("normal-skill").join("SKILL.md"),
            make_skill_md("normal-skill", "Normal", "# NS"),
        )
        .unwrap();

        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(catalog.bootstrap_skills.len(), 1);
        assert_eq!(catalog.bootstrap_skills[0].name, "bootstrap-skill");
    }

    #[test]
    fn bootstrap_convention() {
        let dir = TempDir::new().unwrap();
        let pack_dir = dir.path().join("mypack");
        let plugin_dir = pack_dir.join(".claude-plugin");
        let skills_dir = pack_dir.join("skills");
        fs::create_dir_all(&plugin_dir).unwrap();
        fs::create_dir_all(skills_dir.join("using-mypack")).unwrap();
        fs::create_dir_all(skills_dir.join("other")).unwrap();

        fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"name": "mypack"}"#,
        )
        .unwrap();
        fs::write(
            skills_dir.join("using-mypack").join("SKILL.md"),
            make_skill_md("using-mypack", "Convention bootstrap", "# BS"),
        )
        .unwrap();
        fs::write(
            skills_dir.join("other").join("SKILL.md"),
            make_skill_md("other", "Other", "# O"),
        )
        .unwrap();

        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(catalog.bootstrap_skills.len(), 1);
        assert_eq!(catalog.bootstrap_skills[0].name, "using-mypack");
    }

    // --- Resolve 测试 ---

    #[test]
    fn resolve_by_name() {
        let catalog = SkillCatalog {
            skills: vec![SkillMeta {
                name: "brainstorming".into(),
                namespace: None,
                description: "desc".into(),
                when_to_use: None,
                source_path: PathBuf::from("/fake/SKILL.md"),
                pack_name: None,
                is_bootstrap: false,
            }],
            packs: vec![],
            bootstrap_skills: vec![],
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
                pack_name: Some("superpowers".into()),
                is_bootstrap: false,
            }],
            packs: vec![],
            bootstrap_skills: vec![],
        };
        assert!(catalog.resolve("superpowers:brainstorming").is_some());
        assert!(catalog.resolve("brainstorming").is_some());
    }

    #[test]
    fn resolve_pack_level() {
        let catalog = SkillCatalog {
            skills: vec![
                SkillMeta {
                    name: "bootstrap-skill".into(),
                    namespace: Some("pack".into()),
                    description: "BS".into(),
                    when_to_use: None,
                    source_path: PathBuf::from("/fake/bs.md"),
                    pack_name: Some("pack".into()),
                    is_bootstrap: true,
                },
                SkillMeta {
                    name: "other-skill".into(),
                    namespace: Some("pack".into()),
                    description: "OS".into(),
                    when_to_use: None,
                    source_path: PathBuf::from("/fake/os.md"),
                    pack_name: Some("pack".into()),
                    is_bootstrap: false,
                },
            ],
            packs: vec![SkillPackMeta {
                name: "pack".into(),
                description: None,
                version: None,
                root_path: PathBuf::from("/fake"),
                manifest_path: None,
                skills_dir: PathBuf::from("/fake/skills"),
                bootstrap_skill: Some("bootstrap-skill".into()),
            }],
            bootstrap_skills: vec![],
        };
        // 包名匹配 → 返回 bootstrap 技能
        let resolved = catalog.resolve("pack").unwrap();
        assert_eq!(resolved.name, "bootstrap-skill");
    }

    #[test]
    fn resolve_fuzzy() {
        let catalog = SkillCatalog {
            skills: vec![SkillMeta {
                name: "brainstorming".into(),
                namespace: None,
                description: "Force brainstorming before coding".into(),
                when_to_use: None,
                source_path: PathBuf::from("/fake"),
                pack_name: None,
                is_bootstrap: false,
            }],
            packs: vec![],
            bootstrap_skills: vec![],
        };
        // 模糊匹配 name
        assert!(catalog.resolve("brain").is_some());
        // 模糊匹配 description
        assert!(catalog.resolve("coding").is_some());
    }

    #[test]
    fn resolve_pack_method() {
        let catalog = SkillCatalog {
            skills: vec![
                SkillMeta {
                    name: "a".into(),
                    namespace: Some("pack".into()),
                    description: "A".into(),
                    when_to_use: None,
                    source_path: PathBuf::from("/fake/a.md"),
                    pack_name: Some("pack".into()),
                    is_bootstrap: false,
                },
                SkillMeta {
                    name: "b".into(),
                    namespace: Some("pack".into()),
                    description: "B".into(),
                    when_to_use: None,
                    source_path: PathBuf::from("/fake/b.md"),
                    pack_name: Some("pack".into()),
                    is_bootstrap: false,
                },
                SkillMeta {
                    name: "c".into(),
                    namespace: None,
                    description: "Standalone".into(),
                    when_to_use: None,
                    source_path: PathBuf::from("/fake/c.md"),
                    pack_name: None,
                    is_bootstrap: false,
                },
            ],
            packs: vec![],
            bootstrap_skills: vec![],
        };
        let pack_skills = catalog.resolve_pack("pack");
        assert_eq!(pack_skills.len(), 2);
    }

    // --- summary_for_prompt 测试 ---

    #[test]
    fn summary_for_prompt_xml() {
        let catalog = SkillCatalog {
            skills: vec![SkillMeta {
                name: "brainstorming".into(),
                namespace: None,
                description: "Force brainstorming".into(),
                when_to_use: None,
                source_path: PathBuf::from("/fake"),
                pack_name: None,
                is_bootstrap: false,
            }],
            packs: vec![],
            bootstrap_skills: vec![],
        };
        let xml = catalog.summary_for_prompt();
        assert!(xml.contains("<available_skills>"));
        assert!(xml.contains("<name>brainstorming</name>"));
    }

    #[test]
    fn summary_includes_packs() {
        let catalog = SkillCatalog {
            skills: vec![SkillMeta {
                name: "brain".into(),
                namespace: Some("superpowers".into()),
                description: "Brain skill".into(),
                when_to_use: None,
                source_path: PathBuf::from("/fake"),
                pack_name: Some("superpowers".into()),
                is_bootstrap: false,
            }],
            packs: vec![SkillPackMeta {
                name: "superpowers".into(),
                description: Some("Core skills lib".into()),
                version: Some("5.1.0".into()),
                root_path: PathBuf::from("/fake"),
                manifest_path: None,
                skills_dir: PathBuf::from("/fake/skills"),
                bootstrap_skill: None,
            }],
            bootstrap_skills: vec![],
        };
        let xml = catalog.summary_for_prompt();
        assert!(xml.contains("<skill_packs>"));
        assert!(xml.contains("superpowers"));
        assert!(xml.contains("<skill_count>1</skill_count>"));
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

        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        let content = catalog.load_content(&catalog.skills[0]).unwrap();
        assert!(content.contains("# TDD Rules"));
        assert!(!content.contains("name: tdd"));
    }
}
