use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

const DEFAULT_SKILL_SUMMARY_CHAR_BUDGET: usize = 8_000;
const MAX_SKILL_SCAN_DEPTH: usize = 8;

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

#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    #[serde(default, alias = "when-to-use")]
    when_to_use: Option<String>,
    #[serde(default)]
    bootstrap: bool,
}

/// One filesystem root participating in skill discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRoot {
    path: PathBuf,
    allow_bootstrap: bool,
}

impl SkillRoot {
    /// Native AI Brain roots may explicitly opt a skill into startup bootstrap.
    pub fn native(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            allow_bootstrap: true,
        }
    }

    /// Compatibility roots are discovered progressively and never auto-bootstrap.
    pub fn external(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            allow_bootstrap: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Resolves one immutable skill catalog per execution working directory.
#[derive(Debug)]
pub struct SkillCatalogResolver {
    global_roots: Vec<SkillRoot>,
    cache: RwLock<HashMap<PathBuf, Arc<SkillCatalog>>>,
}

impl SkillCatalogResolver {
    pub fn new(global_roots: Vec<SkillRoot>) -> Self {
        Self {
            global_roots,
            cache: RwLock::new(HashMap::new()),
        }
    }

    pub fn catalog_for(&self, working_directory: &Path) -> Result<Arc<SkillCatalog>, String> {
        let cache_key = working_directory
            .canonicalize()
            .unwrap_or_else(|_| working_directory.to_path_buf());
        if let Some(catalog) = self
            .cache
            .read()
            .map_err(|_| "技能目录缓存读锁已损坏".to_string())?
            .get(&cache_key)
            .cloned()
        {
            return Ok(catalog);
        }

        let mut roots = project_skill_roots(&cache_key);
        roots.extend(self.global_roots.iter().cloned());
        let catalog = Arc::new(SkillCatalog::scan_roots(&roots)?);
        self.cache
            .write()
            .map_err(|_| "技能目录缓存写锁已损坏".to_string())?
            .insert(cache_key, Arc::clone(&catalog));
        Ok(catalog)
    }

    pub fn clear_cache(&self) -> Result<(), String> {
        self.cache
            .write()
            .map_err(|_| "技能目录缓存写锁已损坏".to_string())?
            .clear();
        Ok(())
    }
}

/// Discover project roots from the working directory through the Git root.
pub fn project_skill_roots(working_directory: &Path) -> Vec<SkillRoot> {
    let start = working_directory
        .canonicalize()
        .unwrap_or_else(|_| working_directory.to_path_buf());
    let mut ancestors = Vec::new();
    let mut found_repository_root = false;
    for ancestor in start.ancestors() {
        ancestors.push(ancestor.to_path_buf());
        if ancestor.join(".git").exists() {
            found_repository_root = true;
            break;
        }
    }
    if !found_repository_root {
        ancestors.truncate(1);
    }

    let mut roots = Vec::new();
    for ancestor in ancestors {
        roots.push(SkillRoot::native(ancestor.join(".ai-brain/skills")));
        roots.push(SkillRoot::external(ancestor.join(".agents/skills")));
        roots.push(SkillRoot::external(ancestor.join(".claude/skills")));
        roots.push(SkillRoot::external(ancestor.join(".codex/skills")));
    }
    roots
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
    pub fn scan_all(roots: &[PathBuf]) -> Result<Self, String> {
        let roots = roots
            .iter()
            .cloned()
            .map(SkillRoot::native)
            .collect::<Vec<_>>();
        Self::scan_roots(&roots)
    }

    /// Scan native and compatibility roots in precedence order.
    ///
    /// Direct skill folders and nested skill packs are followed recursively.
    /// Canonical paths prevent duplicate registration through symlinks, while
    /// the first root still wins lookup precedence.
    pub fn scan_roots(roots: &[SkillRoot]) -> Result<Self, String> {
        let mut scanner = SkillScanner::default();
        for root in roots {
            scanner.scan_root(root);
        }
        Ok(scanner.finish())
    }

    /// 解析查询：支持 4 层匹配
    ///
    /// 1. 命名空间限定: "superpowers:brainstorming" → 精确匹配
    /// 2. 精确名称: "brainstorming" → 先无 namespace，再 fallback
    /// 3. 包名匹配: "superpowers" → 返回 bootstrap 或第一个子技能
    /// 4. 模糊匹配: "brain" → name 或 description 包含关键词
    pub fn resolve(&self, query: &str) -> Option<&SkillMeta> {
        // An exact path disambiguates skills with the same name across scopes.
        if let Some(exact_path) = self
            .skills
            .iter()
            .find(|skill| skill.source_path.to_string_lossy() == query)
        {
            return Some(exact_path);
        }

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
                if let Some(bs) = self
                    .skills
                    .iter()
                    .find(|s| s.pack_name.as_deref() == Some(&pack.name) && s.name == *bs_name)
                {
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
        self.skills.iter().find(|s| {
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

    /// Load a selected skill and identify the directory used to resolve its
    /// bundled scripts, references, and assets.
    pub fn load_for_tool(&self, meta: &SkillMeta) -> Result<String, String> {
        let body = self.load_content(meta)?;
        let directory = meta
            .source_path
            .parent()
            .ok_or_else(|| "SKILL.md 缺少父目录".to_string())?;
        Ok(format!(
            "Skill directory: {}\nSkill file: {}\n\n{}",
            directory.display(),
            meta.source_path.display(),
            body
        ))
    }

    /// Compose every trusted native bootstrap skill without allowing one body
    /// to overwrite another.
    pub fn bootstrap_content(&self) -> Result<String, String> {
        let mut sections = Vec::with_capacity(self.bootstrap_skills.len());
        for skill in &self.bootstrap_skills {
            sections.push(format!(
                "<bootstrap_skill name=\"{}\">\n{}\n</bootstrap_skill>",
                xml_escape(&skill.name),
                self.load_for_tool(skill)?
            ));
        }
        Ok(sections.join("\n\n"))
    }

    /// 生成 <available_skills> XML 段，注入 system prompt
    pub fn summary_for_prompt(&self) -> String {
        self.summary_for_prompt_with_budget(DEFAULT_SKILL_SUMMARY_CHAR_BUDGET)
    }

    /// Generate bounded startup metadata. Full instructions remain available
    /// through `load_for_tool`, even when a skill is omitted from this list.
    pub fn summary_for_prompt_with_budget(&self, budget: usize) -> String {
        bounded_skill_summary(self, budget)
    }
}

#[derive(Debug, Clone)]
struct PackContext {
    name: String,
    bootstrap_skill: Option<String>,
}

#[derive(Default)]
struct SkillScanner {
    skills: Vec<SkillMeta>,
    packs: Vec<SkillPackMeta>,
    seen_skill_files: HashSet<PathBuf>,
    seen_directories: HashSet<PathBuf>,
    seen_packs: HashSet<PathBuf>,
}

impl SkillScanner {
    fn scan_root(&mut self, root: &SkillRoot) {
        let Ok(root_path) = root.path.canonicalize() else {
            return;
        };
        if !root_path.is_dir() || !self.seen_directories.insert(root_path.clone()) {
            return;
        }

        if root_path.join("SKILL.md").is_file() || has_manifest_candidate(&root_path) {
            self.seen_directories.remove(&root_path);
            self.walk_directory(&root_path, 0, root.allow_bootstrap, None);
            return;
        }

        for child in child_directories(&root_path) {
            let direct_or_structured =
                child.join("SKILL.md").is_file() || has_manifest_candidate(&child);
            if direct_or_structured {
                self.walk_directory(&child, 1, root.allow_bootstrap, None);
                continue;
            }

            let Some(name) = child
                .file_name()
                .map(|value| value.to_string_lossy().to_string())
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            let canonical = child.canonicalize().unwrap_or_else(|_| child.clone());
            let bootstrap_skill = root
                .allow_bootstrap
                .then(|| detect_bootstrap_skill(&canonical, &name))
                .flatten();
            let context = PackContext {
                name: name.clone(),
                bootstrap_skill: bootstrap_skill.clone(),
            };
            let count = self.walk_directory(&canonical, 1, root.allow_bootstrap, Some(&context));
            if count > 0 && self.seen_packs.insert(canonical.clone()) {
                self.packs.push(SkillPackMeta {
                    name,
                    description: None,
                    version: None,
                    root_path: canonical.clone(),
                    manifest_path: None,
                    skills_dir: canonical,
                    bootstrap_skill,
                });
            }
        }
    }

    fn walk_directory(
        &mut self,
        directory: &Path,
        depth: usize,
        allow_bootstrap: bool,
        pack: Option<&PackContext>,
    ) -> usize {
        if depth > MAX_SKILL_SCAN_DEPTH {
            return 0;
        }
        let Ok(directory) = directory.canonicalize() else {
            return 0;
        };
        if !directory.is_dir() || !self.seen_directories.insert(directory.clone()) {
            return 0;
        }

        let skill_file = directory.join("SKILL.md");
        if skill_file.is_file() {
            return usize::from(self.register_skill(&skill_file, allow_bootstrap, pack));
        }

        if has_manifest_candidate(&directory) {
            let Some(structured) = detect_structured_pack(&directory) else {
                tracing::warn!(
                    path = %directory.display(),
                    "技能包 manifest 无效，已跳过该目录"
                );
                return 0;
            };
            self.scan_structured_pack(structured, depth, allow_bootstrap);
            return 0;
        }

        child_directories(&directory)
            .into_iter()
            .map(|child| self.walk_directory(&child, depth + 1, allow_bootstrap, pack))
            .sum()
    }

    fn scan_structured_pack(
        &mut self,
        mut pack: SkillPackMeta,
        depth: usize,
        allow_bootstrap: bool,
    ) {
        let root_path = pack
            .root_path
            .canonicalize()
            .unwrap_or_else(|_| pack.root_path.clone());
        let skills_dir = pack
            .skills_dir
            .canonicalize()
            .unwrap_or_else(|_| pack.skills_dir.clone());
        let bootstrap_skill = allow_bootstrap
            .then(|| detect_bootstrap_skill(&skills_dir, &pack.name))
            .flatten();
        pack.root_path.clone_from(&root_path);
        pack.skills_dir.clone_from(&skills_dir);
        pack.manifest_path = pack
            .manifest_path
            .and_then(|path| path.canonicalize().ok().or(Some(path)));
        pack.bootstrap_skill.clone_from(&bootstrap_skill);

        if self.seen_packs.insert(root_path.clone()) {
            self.packs.push(pack.clone());
        }
        let context = PackContext {
            name: pack.name,
            bootstrap_skill,
        };
        self.walk_directory(&skills_dir, depth + 1, allow_bootstrap, Some(&context));
    }

    fn register_skill(
        &mut self,
        skill_file: &Path,
        allow_bootstrap: bool,
        pack: Option<&PackContext>,
    ) -> bool {
        let Ok(source_path) = skill_file.canonicalize() else {
            return false;
        };
        if !self.seen_skill_files.insert(source_path.clone()) {
            return false;
        }
        let Some(parsed) = parse_skill_file(&source_path) else {
            tracing::warn!(path = %source_path.display(), "无法解析技能元数据，已跳过");
            return false;
        };
        let is_bootstrap = allow_bootstrap
            && (parsed.bootstrap
                || pack
                    .and_then(|value| value.bootstrap_skill.as_deref())
                    .is_some_and(|name| name == parsed.name));
        self.skills.push(SkillMeta {
            name: parsed.name,
            namespace: pack
                .map(|value| value.name.clone())
                .or_else(|| infer_namespace(&source_path)),
            description: parsed.description,
            when_to_use: parsed.when_to_use,
            source_path,
            pack_name: pack.map(|value| value.name.clone()),
            is_bootstrap,
        });
        true
    }

    fn finish(mut self) -> SkillCatalog {
        for pack in &mut self.packs {
            if pack.bootstrap_skill.is_none() {
                pack.bootstrap_skill = self
                    .skills
                    .iter()
                    .find(|skill| {
                        skill.is_bootstrap
                            && skill.source_path.starts_with(&pack.skills_dir)
                            && skill.pack_name.as_deref() == Some(&pack.name)
                    })
                    .map(|skill| skill.name.clone());
            }
        }
        let bootstrap_skills = self
            .skills
            .iter()
            .filter(|skill| skill.is_bootstrap)
            .cloned()
            .collect::<Vec<_>>();
        tracing::info!(
            "扫描完成: {} 个技能, {} 个技能包, {} 个 bootstrap 技能",
            self.skills.len(),
            self.packs.len(),
            bootstrap_skills.len()
        );
        SkillCatalog {
            skills: self.skills,
            packs: self.packs,
            bootstrap_skills,
        }
    }
}

fn child_directories(directory: &Path) -> Vec<PathBuf> {
    let mut children = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| std::fs::metadata(path).is_ok_and(|metadata| metadata.is_dir()))
        .collect::<Vec<_>>();
    children.sort_by(|left, right| left.as_os_str().cmp(right.as_os_str()));
    children
}

fn bounded_skill_summary(catalog: &SkillCatalog, budget: usize) -> String {
    if catalog.skills.is_empty() && catalog.packs.is_empty() {
        return String::new();
    }

    let open = "\n<available_skills>\n";
    let close = "</available_skills>\n";
    let wrapper_chars = open.chars().count() + close.chars().count();
    if budget < wrapper_chars {
        return String::new();
    }
    let pack_section = pack_summary(catalog);
    let entries = catalog
        .skills
        .iter()
        .map(skill_summary_entry)
        .collect::<Vec<_>>();
    let full = format!("{open}{pack_section}{}{close}", entries.concat());
    if full.chars().count() <= budget {
        return full;
    }

    let mut summary = open.to_string();
    let mut included = 0usize;
    for entry in &entries {
        let omitted = entries.len().saturating_sub(included + 1);
        let marker = omitted_marker(omitted);
        let candidate_chars = summary.chars().count()
            + entry.chars().count()
            + marker.chars().count()
            + close.chars().count();
        if candidate_chars > budget {
            break;
        }
        summary.push_str(entry);
        included += 1;
    }

    let omitted = entries.len().saturating_sub(included);
    let marker = omitted_marker(omitted);
    if summary.chars().count() + marker.chars().count() + close.chars().count() <= budget {
        summary.push_str(&marker);
    }
    summary.push_str(close);
    summary
}

fn pack_summary(catalog: &SkillCatalog) -> String {
    if catalog.packs.is_empty() {
        return String::new();
    }
    let mut summary = String::from("  <skill_packs>\n");
    for pack in &catalog.packs {
        let _ = writeln!(summary, "    <pack name=\"{}\">", xml_escape(&pack.name));
        if let Some(description) = &pack.description {
            let _ = writeln!(
                summary,
                "      <description>{}</description>",
                xml_escape(description)
            );
        }
        let count = catalog
            .skills
            .iter()
            .filter(|skill| skill.pack_name.as_deref() == Some(&pack.name))
            .count();
        let _ = writeln!(summary, "      <skill_count>{count}</skill_count>");
        summary.push_str("    </pack>\n");
    }
    summary.push_str("  </skill_packs>\n");
    summary
}

fn skill_summary_entry(skill: &SkillMeta) -> String {
    const DESCRIPTION_LIMIT: usize = 480;
    let mut summary = String::from("  <skill>\n");
    let display = skill.namespace.as_ref().map_or_else(
        || skill.name.clone(),
        |namespace| format!("{namespace}:{}", skill.name),
    );
    let _ = writeln!(summary, "    <name>{}</name>", xml_escape(&display));
    let description = truncate_with_ellipsis(&skill.description, DESCRIPTION_LIMIT);
    let _ = writeln!(
        summary,
        "    <description>{}</description>",
        xml_escape(&description)
    );
    if let Some(when) = &skill.when_to_use {
        let _ = writeln!(
            summary,
            "    <when_to_use>{}</when_to_use>",
            xml_escape(when)
        );
    }
    let _ = writeln!(
        summary,
        "    <location>{}</location>",
        xml_escape(&skill.source_path.display().to_string())
    );
    summary.push_str("  </skill>\n");
    summary
}

fn omitted_marker(count: usize) -> String {
    if count > 0 {
        format!("  <omitted count=\"{count}\" />\n")
    } else {
        String::new()
    }
}

fn truncate_with_ellipsis(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut shortened = value
        .chars()
        .take(limit.saturating_sub(3))
        .collect::<String>();
    shortened.push_str("...");
    shortened
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn has_manifest_candidate(directory: &Path) -> bool {
    MANIFEST_PATHS
        .iter()
        .any(|relative| directory.join(relative).is_file())
}

/// 从文件路径推断插件命名空间。
/// 路径格式: .../plugins/cache/{publisher}/{plugin}/{version}/skills/{name}/SKILL.md
fn infer_namespace(path: &Path) -> Option<String> {
    let components = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    let cache = components
        .iter()
        .rposition(|component| component == "cache")?;
    let plugin = components.get(cache + 2)?;
    let version = components.get(cache + 3)?;
    let skills = components.get(cache + 4)?;
    if plugin.is_empty() || version.is_empty() || skills != "skills" {
        return None;
    }
    Some(plugin.clone())
}

/// 解析 SKILL.md 文件
fn parse_skill_file(path: &Path) -> Option<ParsedSkill> {
    let content = std::fs::read_to_string(path).ok()?;
    parse_skill_content(&content)
}

/// 解析 SKILL.md 内容：frontmatter + body
fn parse_skill_content(content: &str) -> Option<ParsedSkill> {
    let trimmed = content
        .trim_start_matches(|character: char| character.is_whitespace() || character == '\u{feff}');
    let after_open = trimmed.strip_prefix("---")?;
    if !after_open.starts_with('\n') && !after_open.starts_with("\r\n") {
        return None;
    }
    let rest = after_open.trim_start_matches(['\n', '\r']);
    let mut offset = 0usize;
    let mut sections = None;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            sections = Some((&rest[..offset], &rest[offset + line.len()..]));
            break;
        }
        offset += line.len();
    }
    if sections.is_none() && rest[offset..].trim_end_matches('\r') == "---" {
        sections = Some((&rest[..offset], ""));
    }
    let (frontmatter, body) = sections?;
    let frontmatter = serde_yaml::from_str::<SkillFrontmatter>(frontmatter).ok()?;
    let name = frontmatter.name.trim().to_string();
    let description = frontmatter.description;
    if name.is_empty() || description.trim().is_empty() {
        return None;
    }

    Some(ParsedSkill {
        name,
        description,
        when_to_use: frontmatter.when_to_use,
        bootstrap: frontmatter.bootstrap,
        body: body.trim().to_string(),
    })
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

    fn make_skill_md_with_bootstrap(
        name: &str,
        description: &str,
        bootstrap: bool,
        body: &str,
    ) -> String {
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
        let result = parse_skill_content(content).unwrap();
        assert_eq!(result.name, "tdd");
        assert_eq!(result.when_to_use, Some("when writing code".to_string()));
    }

    #[test]
    fn parse_skill_quoted_values() {
        let content = "---\nname: 'my-skill'\ndescription: \"A skill\"\n---\nbody";
        let result = parse_skill_content(content).unwrap();
        assert_eq!(result.name, "my-skill");
    }

    #[test]
    fn parse_skill_bootstrap_field() {
        let content = make_skill_md_with_bootstrap(
            "using-superpowers",
            "Bootstrap skill",
            true,
            "# Bootstrap",
        );
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
    fn scan_nested_single_skill_is_discovered() {
        let dir = TempDir::new().unwrap();
        let pack_dir = dir.path().join("lonely-pack");
        fs::create_dir_all(pack_dir.join("only-skill")).unwrap();
        fs::write(
            pack_dir.join("only-skill").join("SKILL.md"),
            make_skill_md("only-skill", "Only one", "# Content"),
        )
        .unwrap();

        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        assert_eq!(catalog.packs.len(), 1);
        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(catalog.skills[0].name, "only-skill");
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
            SkillCatalog::scan_all(&[dir.path().to_path_buf(), dir.path().to_path_buf()]).unwrap();
        assert_eq!(catalog.skills.len(), 1);
    }

    #[test]
    fn earlier_roots_win_names_and_exact_paths_disambiguate_collisions() {
        let dir = TempDir::new().unwrap();
        let project = dir.path().join("project");
        let global = dir.path().join("global");
        for (root, body) in [(&project, "# Project"), (&global, "# Global")] {
            let skill_dir = root.join("same-name");
            fs::create_dir_all(&skill_dir).unwrap();
            fs::write(
                skill_dir.join("SKILL.md"),
                make_skill_md("same-name", "Collision", body),
            )
            .unwrap();
        }

        let catalog = SkillCatalog::scan_roots(&[
            SkillRoot::external(&project),
            SkillRoot::external(&global),
        ])
        .unwrap();
        assert_eq!(catalog.skills.len(), 2);
        assert!(catalog
            .load_content(catalog.resolve("same-name").unwrap())
            .unwrap()
            .contains("# Project"));

        let global_path = global.join("same-name/SKILL.md").canonicalize().unwrap();
        assert!(catalog
            .load_content(catalog.resolve(&global_path.display().to_string()).unwrap())
            .unwrap()
            .contains("# Global"));
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

            fs::write(pack_dir.join(rel), r#"{"name": "test-pack"}"#).unwrap();
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
        fs::write(plugin_dir.join("plugin.json"), r#"{"name": "bad/name"}"#).unwrap();
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

        fs::write(plugin_dir.join("plugin.json"), r#"{"name": "pack"}"#).unwrap();
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

        fs::write(plugin_dir.join("plugin.json"), r#"{"name": "mypack"}"#).unwrap();
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

    #[test]
    fn parses_multiline_yaml_description() {
        let content = "---\nname: open-web\ndescription: |\n  Search the public web.\n  Use for current facts.\nmetadata:\n  owner: community\n---\n# Open Web";
        let parsed = parse_skill_content(content).unwrap();
        assert_eq!(parsed.name, "open-web");
        assert_eq!(
            parsed.description,
            "Search the public web.\nUse for current facts.\n"
        );
        assert_eq!(parsed.body, "# Open Web");
    }

    #[test]
    fn external_roots_never_enable_bootstrap() {
        let dir = TempDir::new().unwrap();
        let skill_dir = dir.path().join("foreign-bootstrap");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            make_skill_md_with_bootstrap(
                "foreign-bootstrap",
                "Must remain on demand",
                true,
                "# Foreign",
            ),
        )
        .unwrap();

        let catalog = SkillCatalog::scan_roots(&[SkillRoot::external(dir.path())]).unwrap();
        assert_eq!(catalog.skills.len(), 1);
        assert!(!catalog.skills[0].is_bootstrap);
        assert!(catalog.bootstrap_skills.is_empty());
    }

    #[test]
    fn native_direct_skill_can_bootstrap_and_multiple_bodies_are_composed() {
        let dir = TempDir::new().unwrap();
        for (name, body) in [("first", "# First"), ("second", "# Second")] {
            let skill_dir = dir.path().join(name);
            fs::create_dir_all(&skill_dir).unwrap();
            fs::write(
                skill_dir.join("SKILL.md"),
                make_skill_md_with_bootstrap(name, "Native bootstrap", true, body),
            )
            .unwrap();
        }

        let catalog = SkillCatalog::scan_roots(&[SkillRoot::native(dir.path())]).unwrap();
        assert_eq!(catalog.bootstrap_skills.len(), 2);
        let bootstrap = catalog.bootstrap_content().unwrap();
        assert!(bootstrap.contains("# First"));
        assert!(bootstrap.contains("# Second"));
    }

    #[test]
    fn selected_skill_content_includes_canonical_base_directory() {
        let dir = TempDir::new().unwrap();
        let skill_dir = dir.path().join("with-assets");
        fs::create_dir_all(skill_dir.join("references")).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            make_skill_md(
                "with-assets",
                "Uses bundled files",
                "Read references/guide.md",
            ),
        )
        .unwrap();

        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        let loaded = catalog
            .load_for_tool(catalog.resolve("with-assets").unwrap())
            .unwrap();
        assert!(loaded.contains("Skill directory:"));
        assert!(loaded.contains(&skill_dir.display().to_string()));
        assert!(loaded.contains("Read references/guide.md"));
    }

    #[test]
    fn summary_contains_paths_escapes_xml_and_respects_budget() {
        let dir = TempDir::new().unwrap();
        for index in 0..8 {
            let skill_dir = dir.path().join(format!("skill-{index}"));
            fs::create_dir_all(&skill_dir).unwrap();
            fs::write(
                skill_dir.join("SKILL.md"),
                make_skill_md(
                    &format!("skill-{index}"),
                    "Use when x < y & output needs current context",
                    "# Body",
                ),
            )
            .unwrap();
        }

        let catalog = SkillCatalog::scan_all(&[dir.path().to_path_buf()]).unwrap();
        let summary = catalog.summary_for_prompt_with_budget(900);
        assert!(summary.chars().count() <= 900);
        assert!(summary.contains("<location>"));
        assert!(summary.contains("&lt;"));
        assert!(summary.contains("&amp;"));
        assert!(summary.contains("<omitted count="));
    }

    #[test]
    fn summary_never_returns_truncated_xml_for_tiny_budgets() {
        let catalog = SkillCatalog {
            skills: vec![SkillMeta {
                name: "bounded".into(),
                namespace: None,
                description: "Bounded summary".into(),
                when_to_use: None,
                source_path: PathBuf::from("/fake/SKILL.md"),
                pack_name: None,
                is_bootstrap: false,
            }],
            packs: vec![],
            bootstrap_skills: vec![],
        };
        let wrapper = "\n<available_skills>\n</available_skills>\n";
        let wrapper_chars = wrapper.chars().count();

        assert!(catalog
            .summary_for_prompt_with_budget(wrapper_chars - 1)
            .is_empty());
        assert_eq!(
            catalog.summary_for_prompt_with_budget(wrapper_chars),
            wrapper
        );
    }

    #[test]
    fn resolver_combines_room_ancestors_without_cross_room_leakage() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        let room_a = repo.join("services/a");
        let room_b = repo.join("services/b");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(&room_a).unwrap();
        fs::create_dir_all(&room_b).unwrap();

        let global_root = dir.path().join("global");
        let global_skill = global_root.join("global-skill");
        fs::create_dir_all(&global_skill).unwrap();
        fs::write(
            global_skill.join("SKILL.md"),
            make_skill_md("global-skill", "Global", "# Global"),
        )
        .unwrap();

        for (room, name) in [(&room_a, "room-a"), (&room_b, "room-b")] {
            let skill_dir = room.join(".agents/skills").join(name);
            fs::create_dir_all(&skill_dir).unwrap();
            fs::write(
                skill_dir.join("SKILL.md"),
                make_skill_md(name, "Room scoped", "# Room"),
            )
            .unwrap();
        }

        let resolver = SkillCatalogResolver::new(vec![SkillRoot::external(global_root)]);
        let catalog_a = resolver.catalog_for(&room_a).unwrap();
        let catalog_b = resolver.catalog_for(&room_b).unwrap();
        assert!(catalog_a.resolve("global-skill").is_some());
        assert!(catalog_a.resolve("room-a").is_some());
        assert!(catalog_a.resolve("room-b").is_none());
        assert!(catalog_b.resolve("global-skill").is_some());
        assert!(catalog_b.resolve("room-b").is_some());
        assert!(catalog_b.resolve("room-a").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_skill_is_followed_and_deduplicated_by_target() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let target = dir.path().join("target/skill");
        fs::create_dir_all(&target).unwrap();
        fs::write(
            target.join("SKILL.md"),
            make_skill_md("linked", "Linked skill", "# Linked"),
        )
        .unwrap();
        let root_a = dir.path().join("root-a");
        let root_b = dir.path().join("root-b");
        fs::create_dir_all(&root_a).unwrap();
        fs::create_dir_all(&root_b).unwrap();
        symlink(&target, root_a.join("linked-a")).unwrap();
        symlink(&target, root_b.join("linked-b")).unwrap();

        let catalog =
            SkillCatalog::scan_roots(&[SkillRoot::external(root_a), SkillRoot::external(root_b)])
                .unwrap();
        assert_eq!(catalog.skills.len(), 1);
    }
}
