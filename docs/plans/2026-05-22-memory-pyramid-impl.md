# 记忆金字塔重构实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 将 brain-memory crate 从追加式存储重构为原生多人格+金字塔提炼式记忆系统。

**Architecture:** 原生人格系统(注册表+CRUD+配置+prompt注入) + 四层金字塔(L1全量基座→L2任务摘要池→L3经验抽象层→L4潜意识触发词)，每层全量重生成而非追加。多人格完全隔离，每人格独立目录。渐进式召回(LLM驱动，自顶向下)。人格系统不依赖任何外部 MCP。

**Tech Stack:** Rust, serde_json, chrono, tokio(async), brain-llm(LLM trait)

**设计文档:** `docs/plans/2026-05-22-memory-pyramid-redesign.md`

---

## 依赖关系图

```
Task 1 (类型定义 + 人格类型)
  ├─→ Task 2 (原生人格系统)
  │     ├─→ Task 3 (per-persona 存储层)
  │     │     ├─→ Task 4 (L1 Raw Pool)
  │     │     ├─→ Task 5 (L2 Summary Pool)
  │     │     ├─→ Task 6 (L3 Abstraction Layer)
  │     │     ├─→ Task 7 (L4 Subconscious)
  │     │     └─→ Task 8 (Profile + EvalInfo)
  │     └─→ Task 12 (人格命令集成)
  ├─→ Task 9 (Prompt 重写)
  │     └─→ Task 10 (四步浓缩引擎)
  ├─→ Task 11 (渐进式召回引擎)
  └─→ Task 13 (MemoryBrain 重构)
        └─→ Task 14 (Orchestrator 集成)
              └─→ Task 15 (旧数据迁移)
                    └─→ Task 16 (清理旧代码)
```

---

### Task 1: 核心类型定义（含人格类型）

**Files:**
- Create: `rust/crates/brain-memory/src/pyramid_types.rs`
- Create: `rust/crates/brain-memory/src/persona_types.rs`
- Modify: `rust/crates/brain-memory/src/lib.rs`

**设计要点:** 定义金字塔所有层级的数据结构和人格系统类型，这些类型是整个重构的基础。

**Step 1: 定义人格系统类型**

```rust
// persona_types.rs

use serde::{Deserialize, Serialize};

/// 人格定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Persona {
    /// 唯一标识符（英文，如 "cyber-brain", "writer"）
    pub id: String,
    /// 显示名称
    pub name: String,
    /// 人格描述（一段话说明该人格的角色定位）
    pub description: String,
    /// 人格专属 system prompt 片段（注入主脑 prompt）
    pub system_prompt: String,
    /// 人格专属配置
    pub config: PersonaConfig,
    /// 创建时间
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 最后激活时间
    pub last_active_at: chrono::DateTime<chrono::Utc>,
}

/// 人格配置（影响主脑和评估脑行为）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaConfig {
    /// 默认使用的语言（如 "zh-CN", "en"）
    pub language: String,
    /// 输出风格偏好（如 "concise", "detailed", "academic"）
    pub output_style: String,
    /// 额外的模型参数覆盖（可选）
    pub model_override: Option<String>,
    /// 评估脑敏感度（0.0-1.0，越高越严格）
    pub eval_sensitivity: f64,
    /// 四步分析间隔（轮次）
    pub analysis_interval: u32,
}

impl Default for PersonaConfig {
    fn default() -> Self {
        Self {
            language: "zh-CN".into(),
            output_style: "concise".into(),
            model_override: None,
            eval_sensitivity: 0.7,
            analysis_interval: 5,
        }
    }
}

/// 人格注册表（存储在 ~/.ai-brain/personas/registry.json）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaRegistry {
    /// 所有注册的人格
    pub personas: Vec<Persona>,
    /// 当前激活的人格 ID（空字符串 = 默认）
    pub active_persona_id: String,
    /// 更新时间
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl Default for PersonaRegistry {
    fn default() -> Self {
        Self {
            personas: vec![Persona::default_persona()],
            active_persona_id: "default".into(),
            updated_at: chrono::Utc::now(),
        }
    }
}

impl Persona {
    /// 创建默认人格
    pub fn default_persona() -> Self {
        Self {
            id: "default".into(),
            name: "智脑".into(),
            description: "默认人格，通用AI助手".into(),
            system_prompt: String::new(),
            config: PersonaConfig::default(),
            created_at: chrono::Utc::now(),
            last_active_at: chrono::Utc::now(),
        }
    }
}
```

**Step 2: 定义金字塔层级类型（与原 Task 1 相同）**

`pyramid_types.rs` 内容不变，包含 `PyramidLayer`, `TaskSummary`, `L1Ref`, `TaskType`,
`SummaryIndex`, `Experience`, `TypeExperience`, `SubconsciousTrigger`, `SubconsciousData`,
`PersonaProfile`, `EvalInfo` 等所有类型。

**Step 3: 写测试**

```rust
// persona_types.rs tests
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_registry_has_default_persona() {
        let reg = PersonaRegistry::default();
        assert_eq!(reg.personas.len(), 1);
        assert_eq!(reg.active_persona_id, "default");
        assert_eq!(reg.personas[0].id, "default");
    }

    #[test]
    fn persona_config_default_values() {
        let config = PersonaConfig::default();
        assert_eq!(config.language, "zh-CN");
        assert_eq!(config.analysis_interval, 5);
        assert!(config.model_override.is_none());
    }

    #[test]
    fn persona_serde_roundtrip() {
        let p = Persona {
            id: "writer".into(),
            name: "滚开作家".into(),
            description: "网文创作".into(),
            system_prompt: "你是一个网文写作助手...".into(),
            config: PersonaConfig {
                output_style: "literary".into(),
                eval_sensitivity: 0.5,
                ..Default::default()
            },
            created_at: chrono::Utc::now(),
            last_active_at: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: Persona = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "writer");
        assert_eq!(back.config.eval_sensitivity, 0.5);
    }
}
```

**Step 4: 运行测试并提交**

```bash
cargo test -p brain-memory persona_types
cargo test -p brain-memory pyramid_types
git commit -m "feat(memory): 核心类型定义 — 人格系统 + 金字塔层级"
```

---

### Task 2: 原生人格系统

**Files:**
- Create: `rust/crates/brain-memory/src/persona_manager.rs`
- Test: `rust/crates/brain-memory/src/persona_manager.rs` (inline tests)

**设计要点:** 人格注册表的 CRUD 操作，人格切换，人格 prompt 生成。不依赖任何外部 MCP。

**核心结构:**

```rust
// persona_manager.rs

pub struct PersonaManager {
    base_dir: PathBuf,
    registry: PersonaRegistry,
}

impl PersonaManager {
    /// 从磁盘加载或创建默认注册表
    pub fn load_or_create(base_dir: &Path) -> Result<Self, MemoryError> {
        let registry_path = base_dir.join("personas").join("registry.json");
        let registry = if registry_path.exists() {
            let data = std::fs::read_to_string(&registry_path)?;
            serde_json::from_str(&data)?
        } else {
            PersonaRegistry::default()
        };
        Ok(Self { base_dir: base_dir.to_path_buf(), registry })
    }

    /// 列出所有人格
    pub fn list(&self) -> &[Persona] { &self.registry.personas }

    /// 获取当前激活人格
    pub fn active(&self) -> &Persona {
        self.registry.personas.iter()
            .find(|p| p.id == self.registry.active_persona_id)
            .unwrap_or(&self.registry.personas[0])
    }

    /// 获取当前激活人格 ID
    pub fn active_id(&self) -> &str { &self.registry.active_persona_id }

    /// 切换人格
    pub fn switch(&mut self, persona_id: &str) -> Result<&Persona, MemoryError> {
        let persona = self.registry.personas.iter()
            .find(|p| p.id == persona_id)
            .ok_or_else(|| MemoryError::NotFound(format!("人格不存在: {persona_id}")))?;
        self.registry.active_persona_id = persona_id.to_string();
        // 更新最后激活时间
        // 持久化到磁盘
        self.persist()?;
        Ok(self.active())
    }

    /// 创建新人格
    pub fn create(&mut self, id: String, name: String, description: String,
                  system_prompt: String, config: PersonaConfig) -> Result<&Persona, MemoryError> {
        // 检查 ID 唯一性
        if self.registry.personas.iter().any(|p| p.id == id) {
            return Err(MemoryError::Conflict(format!("人格ID已存在: {id}")));
        }
        let persona = Persona {
            id, name, description, system_prompt, config,
            created_at: chrono::Utc::now(),
            last_active_at: chrono::Utc::now(),
        };
        self.registry.personas.push(persona);
        self.persist()?;
        Ok(self.registry.personas.last().unwrap())
    }

    /// 删除人格（不能删除 default 和当前激活的）
    pub fn delete(&mut self, persona_id: &str) -> Result<(), MemoryError> {
        if persona_id == "default" {
            return Err(MemoryError::Conflict("不能删除默认人格".into()));
        }
        if persona_id == self.registry.active_persona_id {
            return Err(MemoryError::Conflict("不能删除当前激活的人格".into()));
        }
        let idx = self.registry.personas.iter().position(|p| p.id == persona_id)
            .ok_or_else(|| MemoryError::NotFound(format!("人格不存在: {persona_id}")))?;
        self.registry.personas.remove(idx);
        // 删除该人格的记忆目录
        let persona_dir = self.base_dir.join("personas").join(persona_id);
        if persona_dir.exists() { std::fs::remove_dir_all(&persona_dir)?; }
        self.persist()?;
        Ok(())
    }

    /// 生成人格的 prompt 注入内容
    pub fn build_persona_prompt(&self) -> String {
        let persona = self.active();
        if persona.system_prompt.is_empty() {
            return String::new();
        }
        format!(
            "[人格: {}]\n{}\n[语言: {}, 风格: {}]",
            persona.name, persona.system_prompt,
            persona.config.language, persona.config.output_style
        )
    }

    /// 获取当前人格的分析间隔配置
    pub fn analysis_interval(&self) -> u32 {
        self.active().config.analysis_interval
    }

    /// 获取当前人格的评估敏感度
    pub fn eval_sensitivity(&self) -> f64 {
        self.active().config.eval_sensitivity
    }

    fn persist(&self) -> Result<(), MemoryError> {
        let dir = self.base_dir.join("personas");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("registry.json");
        let json = serde_json::to_string_pretty(&self.registry)?;
        std::fs::write(path, json)?;
        Ok(())
    }
}
```

**测试:**

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn create_and_switch_persona() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = PersonaManager::load_or_create(tmp.path()).unwrap();
        assert_eq!(mgr.active_id(), "default");

        mgr.create("writer".into(), "作家".into(), "网文".into(),
                   "你是网文助手".into(), PersonaConfig::default()).unwrap();
        assert_eq!(mgr.list().len(), 2);

        mgr.switch("writer").unwrap();
        assert_eq!(mgr.active_id(), "writer");
        assert_eq!(mgr.active().name, "作家");
    }

    #[test]
    fn cannot_delete_default_or_active() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = PersonaManager::load_or_create(tmp.path()).unwrap();
        assert!(mgr.delete("default").is_err());

        mgr.create("writer".into(), "作家".into(), "网文".into(),
                   "".into(), PersonaConfig::default()).unwrap();
        mgr.switch("writer").unwrap();
        assert!(mgr.delete("writer").is_err()); // 当前激活的不能删
    }

    #[test]
    fn persona_prompt_includes_config() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = PersonaManager::load_or_create(tmp.path()).unwrap();
        mgr.create("writer".into(), "作家".into(), "网文".into(),
                   "模仿滚开风格".into(), PersonaConfig {
                       language: "zh-CN".into(),
                       output_style: "literary".into(),
                       ..Default::default()
                   }).unwrap();
        mgr.switch("writer").unwrap();
        let prompt = mgr.build_persona_prompt();
        assert!(prompt.contains("滚开风格"));
        assert!(prompt.contains("literary"));
    }

    #[test]
    fn persist_and_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = PersonaManager::load_or_create(tmp.path()).unwrap();
        mgr.create("writer".into(), "作家".into(), "网文".into(),
                   "".into(), PersonaConfig::default()).unwrap();
        mgr.switch("writer").unwrap();

        // 重新加载
        let mgr2 = PersonaManager::load_or_create(tmp.path()).unwrap();
        assert_eq!(mgr2.active_id(), "writer");
        assert_eq!(mgr2.list().len(), 2);
    }

    #[test]
    fn analysis_interval_per_persona() {
        let tmp = tempfile::tempdir().unwrap();
        let mut mgr = PersonaManager::load_or_create(tmp.path()).unwrap();
        assert_eq!(mgr.analysis_interval(), 5); // default

        mgr.create("writer".into(), "作家".into(), "网文".into(),
                   "".into(), PersonaConfig {
                       analysis_interval: 20,
                       ..Default::default()
                   }).unwrap();
        mgr.switch("writer").unwrap();
        assert_eq!(mgr.analysis_interval(), 20);
    }
}
```

**提交:** `feat(memory): 原生人格系统 — 注册表 + CRUD + 配置 + prompt 注入`

---

### Task 3: Per-Persona 存储层

```rust
// pyramid_types.rs

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 人格标识（空字符串 = 默认人格）
pub type PersonaId = String;

/// 金字塔层级
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PyramidLayer {
    /// L1: 全量记忆基座
    Raw,
    /// L2: 记忆摘要池（按任务分类）
    Summary,
    /// L3: 记忆抽象层（按类型汇总经验）
    Abstract,
    /// L4: 潜意识层（触发词）
    Subconscious,
}

/// L2 任务摘要条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub task_id: String,
    pub task_type: TaskType,
    pub task_name: String,
    pub summary: String,
    /// L1 中关联的文件+段落
    pub l1_refs: Vec<L1Ref>,
    pub tags: Vec<String>,
    pub importance: f64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// L1 引用（指向原始记忆的具体段落）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L1Ref {
    pub session: String,
    /// 段落索引（0-based）
    pub paragraphs: Vec<usize>,
}

/// 任务类型分类
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskType {
    Coding,
    Writing,
    Troubleshooting,
    Research,
    Multimedia,
    Configuration,
    Other(String),
}

/// L2 索引（任务→L1 映射）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryIndex {
    pub entries: Vec<SummaryIndexEntry>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryIndexEntry {
    pub task_id: String,
    pub task_type: TaskType,
    pub task_name: String,
    pub tags: Vec<String>,
    pub importance: f64,
}

/// L3 经验条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Experience {
    pub pattern: String,
    pub description: String,
    pub source_tasks: Vec<String>,
    pub frequency: u32,
    /// 是否在启动时注入上下文
    pub injectable: bool,
}

/// L3 类型化经验文件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeExperience {
    pub task_type: TaskType,
    pub experiences: Vec<Experience>,
    pub l2_refs: Vec<String>,
    /// 该类型内的关键词→L2任务映射索引
    pub index: Vec<KeywordIndex>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 关键词索引条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeywordIndex {
    pub keyword: String,
    pub l2_task_ids: Vec<String>,
}

/// L3 索引（类型→L2 映射）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbstractIndex {
    pub entries: Vec<AbstractIndexEntry>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbstractIndexEntry {
    pub task_type: TaskType,
    pub experience_count: usize,
    pub injectable_count: usize,
    pub l2_task_count: usize,
}

/// L4 潜意识触发词
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubconsciousTrigger {
    pub keyword: String,
    pub l3_type: TaskType,
    pub l2_task: String,
}

/// L4 潜意识层
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubconsciousData {
    pub triggers: Vec<SubconsciousTrigger>,
    pub narrative: String,
    pub version: u64,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 用户画像（100字上限）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaProfile {
    pub summary: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 评估脑信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalInfo {
    pub requirements: Vec<String>,
    pub pitfalls: Vec<String>,
    pub rules: Vec<String>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
```

**Step 2: 写测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_type_serde_roundtrip() {
        let t = TaskType::Coding;
        let json = serde_json::to_string(&t).unwrap();
        let back: TaskType = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn task_type_other_preserves_label() {
        let t = TaskType::Other("design".into());
        let json = serde_json::to_string(&t).unwrap();
        let back: TaskType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, TaskType::Other("design".into()));
    }

    #[test]
    fn l1_ref_serde() {
        let r = L1Ref { session: "sess-001".into(), paragraphs: vec![0, 3, 5] };
        let json = serde_json::to_string(&r).unwrap();
        let back: L1Ref = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session, "sess-001");
        assert_eq!(back.paragraphs, vec![0, 3, 5]);
    }

    #[test]
    fn subconscious_data_narrative_length() {
        let data = SubconsciousData {
            triggers: vec![SubconsciousTrigger {
                keyword: "红冲逻辑".into(),
                l3_type: TaskType::Coding,
                l2_task: "task-001".into(),
            }],
            narrative: "用户是Rust全栈开发者".into(),
            version: 1,
            updated_at: chrono::Utc::now(),
        };
        assert!(data.narrative.len() <= 500);
        assert_eq!(data.triggers.len(), 1);
    }
}
```

**Step 3: 运行测试**

```bash
cd rust && cargo test -p brain-memory pyramid_types -- --nocapture
```

**Step 4: 在 lib.rs 中注册模块**

在 `brain-memory/src/lib.rs` 中添加:
```rust
pub mod pyramid_types;
```

**Step 5: 提交**

```bash
git add crates/brain-memory/src/pyramid_types.rs crates/brain-memory/src/lib.rs
git commit -m "feat(memory): 金字塔核心类型定义"
```

---

### Task 4: L1 Raw Pool（适配现有 raw_layer.rs）

**Files:**
- Modify: `rust/crates/brain-memory/src/raw_layer.rs`
- Keep existing API, add persona-aware path

**设计要点:** L1 基本保持现有逻辑，只是路径从 `sessions/` 改为 per-persona 的 `pyramid/l1-raw/`。现有的 `append_turn`, `read_session` 等方法签名不变。

**Step 1: 给 RawLayer 增加 persona_id 参数**

在 `RawLayer::new()` 中接受 `persona_id: String`，构造路径时使用 `PyramidStorage::l1_dir()`。

**Step 2: 写测试验证路径正确**

```rust
#[test]
fn raw_layer_uses_persona_path() {
    let tmp = tempfile::tempdir().unwrap();
    let store = PyramidStorage::new(tmp.path().to_path_buf(), "cyber-brain");
    let mut raw = RawLayer::new(store.l1_dir());
    raw.append_turn("sess-001", "User", "hello", None).unwrap();
    assert!(store.l1_dir().join("sess-001.jsonl").exists());
}
```

**Step 3: 适配并运行测试**

```bash
cargo test -p brain-memory raw_layer
git commit -m "refactor(memory): L1 Raw Pool 适配 per-persona 路径"
```

---

### Task 5: L2 Summary Pool（任务分类摘要）

**Files:**
- Create: `rust/crates/brain-memory/src/summary_pool.rs`

**设计要点:** L2 是按任务分类的摘要，不是按会话。提供 store/load/index 操作，全量重生成模式。

**核心方法:**
- `regenerate(tasks: Vec<TaskSummary>)` — 全量覆盖重写
- `load_all() -> Vec<TaskSummary>`
- `load_index() -> SummaryIndex`
- `find_by_tags(tags: &[String]) -> Vec<&TaskSummary>`

**容量上限:** 无硬上限，但 prompt 中指导 LLM 浓缩到 50 条以内。

**Step 1: 写测试**

```rust
#[test]
fn regenerate_overwrites_old_data() {
    let tmp = tempfile::tempdir().unwrap();
    let store = PyramidStorage::new(tmp.path().to_path_buf(), "test");
    let pool = SummaryPool::new(store.clone());

    // 第一次写入
    pool.regenerate(vec![TaskSummary { task_id: "t-1".into(), ... }]);
    let loaded = pool.load_all().unwrap();
    assert_eq!(loaded.len(), 1);

    // 第二次全量覆盖
    pool.regenerate(vec![
        TaskSummary { task_id: "t-2".into(), ... },
        TaskSummary { task_id: "t-3".into(), ... },
    ]);
    let loaded = pool.load_all().unwrap();
    assert_eq!(loaded.len(), 2);
    assert!(loaded.iter().all(|t| t.task_id != "t-1"));
}

#[test]
fn index_auto_generated() {
    // regenerate 后 index.json 自动生成
    let index = pool.load_index().unwrap();
    assert_eq!(index.entries.len(), 2);
}
```

**Step 2: 实现 SummaryPool**

```rust
pub struct SummaryPool {
    storage: PyramidStorage,
}

impl SummaryPool {
    pub fn new(storage: PyramidStorage) -> Self { Self { storage } }

    /// 全量重生成：覆盖所有 L2 数据
    pub fn regenerate(&self, tasks: Vec<TaskSummary>) -> Result<(), MemoryError> {
        let index = SummaryIndex {
            entries: tasks.iter().map(|t| SummaryIndexEntry {
                task_id: t.task_id.clone(),
                task_type: t.task_type.clone(),
                task_name: t.task_name.clone(),
                tags: t.tags.clone(),
                importance: t.importance,
            }).collect(),
            updated_at: chrono::Utc::now(),
        };
        // 写索引
        self.storage.write_json(&self.storage.l2_dir().join("index.json"), &index)?;
        // 写各任务文件（全量覆盖：先清空目录再写入）
        self.clean_dir(&self.storage.l2_dir())?;
        for task in &tasks {
            let path = self.storage.l2_dir().join(format!("{}.json", task.task_id));
            self.storage.write_json(&path, task)?;
        }
        Ok(())
    }

    pub fn load_all(&self) -> Result<Vec<TaskSummary>, MemoryError> { ... }
    pub fn load_index(&self) -> Result<SummaryIndex, MemoryError> { ... }
    pub fn find_by_tags(&self, tags: &[String]) -> Result<Vec<TaskSummary>, MemoryError> { ... }
}
```

**Step 3: 运行测试并提交**

```bash
cargo test -p brain-memory summary_pool
git commit -m "feat(memory): L2 Summary Pool 任务分类摘要"
```

---

### Task 6: L3 Abstraction Layer（经验抽象层）

**Files:**
- Create: `rust/crates/brain-memory/src/abstract_layer.rs`

**设计要点:** L3 按任务类型存储经验，每个类型一个文件。提供全量重生成和索引查询。

**核心方法:**
- `regenerate(experiences: Vec<TypeExperience>)` — 全量覆盖
- `load_type(type: &TaskType) -> Option<TypeExperience>`
- `load_index() -> AbstractIndex`
- `load_injectable() -> Vec<Experience>` — 返回所有 injectable=true 的经验
- `find_by_keyword(keyword: &str) -> Vec<&TypeExperience>` — 关键词索引查询

**容量上限:** 每个类型最多 10 条经验，超过则 LLM 需要合并浓缩。

**实现模式与 SummaryPool 相同：全量重生成 + 自动索引。**

**提交:** `feat(memory): L3 Abstraction Layer 经验抽象层`

---

### Task 7: L4 Subconscious（增强版潜意识层）

**Files:**
- Modify: `rust/crates/brain-memory/src/subconscious.rs`

**设计要点:** 在现有潜意识叙事基础上增加触发词→索引映射。保持覆盖式更新模式（500字叙事 + 50个触发词上限）。

**改动点:**
1. `SubconsciousNarrative` 结构体改为使用 `pyramid_types::SubconsciousData`
2. 新增 `triggers: Vec<SubconsciousTrigger>` 字段
3. 叙事上限 500 字，触发词上限 50 个
4. 保持现有的 `load()` / `update()` / `match_keywords()` 方法签名

**向后兼容:** 旧格式 `sc-*.json` 和 `narrative.json`（无 triggers 字段）通过 `#[serde(default)]` 兼容。

**提交:** `feat(memory): L4 Subconscious 增加触发词索引映射`

---

### Task 8: Profile + EvalInfo（融合知识层）

**Files:**
- Modify: `rust/crates/brain-memory/src/user_profile.rs` → 重构为 `PersonaProfile`
- Create: `rust/crates/brain-memory/src/eval_info.rs`

**Profile 设计:**
- 从 186 条偏好列表 → 100字以内的 `summary` 文本
- 全量重生成模式（和潜意识一样）
- 存储: `personas/{persona_id}/profile.json`

**EvalInfo 设计:**
- 合并现有的 pitfall + evolution + eval_requirement 为一个文件
- 全量重生成，为评估脑提供简洁信息
- 存储: `personas/{persona_id}/eval-info.json`
- 内容: requirements(top 5) + pitfalls(top 5) + rules(top 3)

**提交:** `feat(memory): Profile + EvalInfo 融合知识层`

---

### Task 9: Prompt 重写

**Files:**
- Modify: `rust/crates/brain-memory/src/prompts.rs`

**设计要点:** 将所有 prompt 从"追加型"改为"浓缩融合型"。

**核心变化:**

| 旧 prompt | 新 prompt |
|-----------|-----------|
| "以下是已有条目，不要重复。请添加新发现。" | "以下是一份现有数据。请基于新会话内容，重新生成一份完整的、更精炼的版本。" |
| 8 个独立 prompt | 4 个融合 prompt（L1→L2, L2→L3, L3→L4, Profile+EvalInfo） |
| 输出: 新增条目列表 | 输出: 完整替换数据 |

**4 个新 prompt:**

1. **STEP1_L1_TO_L2**: 输入 L1 会话内容 + L2 现有索引 → 输出完整 L2 任务摘要列表
2. **STEP2_L2_TO_L3**: 输入 L2 摘要 + L3 现有经验 → 输出完整 L3 经验列表
3. **STEP3_L3_TO_L4**: 输入 L3 经验 + L4 现有触发词 → 输出完整 L4 触发词+叙事
4. **STEP4_PROFILE**: 输入 L1 会话 + 现有画像 → 输出 100 字画像 + 评估信息

**每个 prompt 都包含容量上限指令**，例如:
- "L2 任务摘要最多 50 条，超过请合并相关任务"
- "L3 每个类型最多 10 条经验"
- "L4 触发词最多 50 个，叙事最多 500 字"

**提交:** `refactor(memory): prompt 重写为浓缩融合型`

---

### Task 10: 四步浓缩引擎

**Files:**
- Create: `rust/crates/brain-memory/src/concentration.rs`
- Modify: `rust/crates/brain-memory/src/lib.rs`

**设计要点:** 替代现有 `analyzer.rs` 的 8 步分析，改为 4 步浓缩。

**引擎结构:**

```rust
pub struct ConcentrationEngine {
    llm: Arc<dyn AnalysisLlm>,
    storage: PyramidStorage,
    session_id: String,
}

impl ConcentrationEngine {
    /// 执行四步浓缩
    pub async fn run(&self, conversation_json: &str) -> Result<ConcentrationReport, MemoryError> {
        // Step 1: L1→L2 任务拆分
        self.step1_l1_to_l2(conversation_json).await?;
        // Step 2: L2→L3 经验抽象
        self.step2_l2_to_l3().await?;
        // Step 3: L3→L4 触发词提取
        self.step3_l3_to_l4().await?;
        // Step 4: Profile + EvalInfo
        self.step4_profile(conversation_json).await?;
        Ok(report)
    }
}
```

**每步的逻辑:**
1. 读取当前层现有数据
2. 拼接新会话内容
3. 调用 LLM 生成完整替换数据
4. 解析 LLM 返回
5. 写入存储（覆盖旧数据）

**错误处理:** Step 1 失败则终止；其余步骤失败 warn 继续。

**测试:** 用 mock LLM（返回预设 JSON）测试四步流程。

**提交:** `feat(memory): 四步浓缩引擎`

---

### Task 11: 渐进式召回引擎

**Files:**
- Create: `rust/crates/brain-memory/src/progressive_recall.rs`

**设计要点:** 替代现有 `recall.rs` + `memory_brain.rs` 中的 `gather_all_candidates`。自顶向下渐进召回。

**核心方法:**

```rust
pub struct ProgressiveRecall {
    storage: PyramidStorage,
}

impl ProgressiveRecall {
    /// 自动注入内容（潜意识 + 画像 + injectable 经验）
    pub fn auto_inject(&self) -> Result<InjectContext, MemoryError> {
        let l4 = self.load_subconscious()?;
        let profile = self.load_profile()?;
        let injectable = self.load_injectable_experiences()?;
        Ok(InjectContext { l4, profile, injectable })
    }

    /// 渐进式召回（LLM 驱动）
    /// 返回按层级排列的结果，LLM 每层判断是否需要继续
    pub fn recall(&self, query: &str, max_depth: PyramidLayer) -> Result<Vec<RecallHit>, MemoryError> {
        let mut hits = Vec::new();
        // L4 触发词匹配
        let l4_matches = self.match_triggers(query);
        hits.extend(l4_matches);
        if max_depth == PyramidLayer::Subconscious { return Ok(hits); }

        // L3 经验查找
        let l3_matches = self.find_in_abstract(&l4_matches);
        hits.extend(l3_matches);
        if max_depth == PyramidLayer::Abstract { return Ok(hits); }

        // L2 任务摘要
        let l2_matches = self.find_in_summary(&l3_matches);
        hits.extend(l2_matches);
        if max_depth == PyramidLayer::Summary { return Ok(hits); }

        // L1 原始记忆
        let l1_matches = self.find_in_raw(&l2_matches);
        hits.extend(l1_matches);
        Ok(hits)
    }
}
```

**提交:** `feat(memory): 渐进式召回引擎`

---

### Task 12: 人格命令集成

**Files:**
- Create: `rust/crates/ai-brain-cli/src/command/persona_cmd.rs`
- Modify: `rust/crates/ai-brain-cli/src/command/mod.rs`
- Modify: `rust/crates/ai-brain-cli/src/command/registry.rs`

**设计要点:** 将人格 CRUD 操作注册为智脑原生命令，替代外部 MCP 依赖。

**命令列表:**

| 命令 | 功能 |
|------|------|
| `:persona list` | 列出所有人格及当前激活状态 |
| `:persona switch <id>` | 切换到指定人格 |
| `:persona create` | 交互式创建新人格（输入 id/name/description/prompt） |
| `:persona delete <id>` | 删除指定人格（不可删 default 和当前激活） |
| `:persona info` | 显示当前人格详情（配置、记忆统计） |

**实现方式:**

```rust
// persona_cmd.rs

pub fn build_persona_commands() -> Vec<Command> {
    vec![
        Command::new("persona")
            .description("人格管理")
            .sub_commands(vec![
                SubCommand::new("list", "列出所有人格", persona_list),
                SubCommand::new("switch", "切换人格", persona_switch),
                SubCommand::new("create", "创建人格", persona_create),
                SubCommand::new("delete", "删除人格", persona_delete),
                SubCommand::new("info", "当前人格详情", persona_info),
            ])
    ]
}

fn persona_list(orch: &Orchestrator) -> HandleResult {
    let mem = orch.memory_brain().lock().await;
    let personas = mem.persona_manager().list();
    // 格式化输出：名称 | ID | 激活状态
}

fn persona_switch(orch: &Orchestrator, id: &str) -> HandleResult {
    // 1. 命令系统拦截，不经过 LLM
    // 2. 获取当前记忆脑锁
    let mut mem = orch.memory_brain().lock().await;
    // 3. 记忆脑内部执行切换（flush旧人格 → 加载新人格 → 重建Storage）
    mem.switch_persona(id)?;
    // 4. 通知主脑重置上下文
    let main_brain = orch.main_brain().lock().await;
    main_brain.reset_for_persona_switch();
    // 5. 注入新人格上下文
    let inject = mem.auto_inject()?;
    main_brain.inject_persona_context(&inject);
    // 输出切换成功信息
}
```

---

### Task 12.5: 人格切换时序与上下文隔离保障

**本节为设计约束，约束 Task 12/13/14 的实现。**

#### 命令拦截机制

人格切换命令 (`:persona switch`) 是 CLI 命令，在命令系统层面拦截，**不经过 LLM**：

```
用户输入
  │
  ├─ 以 ":" 开头 → 命令系统拦截 → 代码直接执行，不调 LLM
  │   ":persona switch B"  → 代码切人格（零 LLM 调用）
  │   ":persona list"      → 代码列人格
  │
  └─ 其他 → 发给主脑 → 主脑调 LLM
      "帮我写个函数"  → LLM 处理
```

#### 启动时序（默认注入上次人格）

```
进程启动
  ├─ 1. 加载 registry.json → 读取 active_persona_id（假设为 A）
  ├─ 2. 创建 PyramidStorage(base_dir, "A")
  ├─ 3. 加载 A/pending.json → 注入上下文（如果有未分析数据）
  ├─ 4. auto_inject(): 加载 A 的 L4 + profile + injectable 经验
  ├─ 5. 构建主脑 system prompt = 基础prompt + A的prompt片段 + A的画像 + A的潜意识
  └─ 此时还没调过 LLM，A 的上下文只在内存中
```

#### 场景：启动后第一条消息就切人格

```
进程启动 → 加载 A 的上下文（内存中，未调 LLM）

用户输入: ":persona switch B"
  │
  ├─ 命令系统拦截，不调 LLM
  ├─ 代码执行:
  │   a. flush 当前 pending 到 A/pending.json（如果有未写入数据）
  │   b. 清空内存中 A 的所有注入内容（A 的上下文从未离开过本地进程）
  │   c. registry.json → active_persona_id = "B"
  │   d. 重建 PyramidStorage(base_dir, "B")
  │   e. auto_inject(): 加载 B 的 L4 + profile + injectable 经验
  │   f. 主脑 system prompt 重建 = 基础prompt + B的prompt片段 + B的画像 + B的潜意识
  │   g. 主脑 conversation history 清空
  │   h. 返回 "已切换到人格 B"
  │
  └─ A 的上下文从未发送给任何 LLM，零 token 浪费，零信息泄露

用户输入: "帮我写个函数"
  └─ 这时才构建完整消息 → 用 B 的上下文 → 发给 LLM
      LLM 从头到尾只见过 B 的内容
```

#### 场景：对话中途切人格

```
A 人格活跃，已进行 5 轮对话
  → turns 1-5 存入 A/l1-raw/sess-001.jsonl
  → LLM 已处理过 5 轮（用的是 A 的上下文）

用户: ":persona switch B"
  │
  ├─ 命令拦截
  ├─ a. 将未分析轮次刷到 A/pending.json
  ├─ b. 清空主脑中所有 A 的注入内容:
  │     - 删 system prompt 中 A 的 prompt 片段
  │     - 删 A 的画像、潜意识、经验
  │     - 清空 conversation history
  ├─ c. 加载 B 的完整上下文
  ├─ d. 主脑就像一个全新启动的进程，只知道 B 的人格和记忆
  └─ 返回 "已切换到人格 B"

继续对话... turns 存入 B/l1-raw/sess-001.jsonl
  → 四步浓缩只读 B/l1-raw/ → 只写 B 的 L2/L3/L4
  → 不会碰 A/ 目录
```

#### 隔离保障总结

| 场景 | 隔离机制 |
|------|---------|
| 启动即切换（未调 LLM） | A 的上下文只在内存，从未发送，直接替换 |
| 中途切换（已调 LLM） | 清空 conversation history + 重置 system prompt + 重建 Storage |
| 多进程并发 | 写入路径完全不同（不同目录），registry.json 文件锁保护 |
| 会话恢复 | pending.json 按人格存储（A/pending.json），恢复时只加载对应人格 |
| 记忆分析 | 浓缩引擎只操作 active_storage，不跨目录 |

**核心原则：切换 = 完全重置上下文。删掉旧的全部，换成新的。主脑 conversation history 也清空，等效于新进程。**

---

### Task 13: MemoryBrain 重构

**Files:**
- Modify: `rust/crates/brain-memory/src/memory_brain.rs`

**设计要点:** 用新的金字塔组件替代旧的扁平存储。保持对外接口不变（`store_turns`, `recall_for_context` 等），内部路由到金字塔。通过 `PersonaManager` 获取当前人格，路由到对应的金字塔。

**关键改动:**

1. `MemoryBrainConfig` 新增 `persona_id: String` 和 `analysis_interval: u32`
2. 内部持有 `PersonaManager` + `PyramidStorage`（根据 active persona 动态）
3. `store_turns` → 写入 L1（不再写 L2 短期记忆）
4. `tick_and_should_analyze` → 触发 `ConcentrationEngine::run`，interval 从 PersonaConfig 读取
5. `recall_for_context` → 委托 `ProgressiveRecall::recall`
6. `load_subconscious_summary` → 从 L4 读取 + 人格 prompt 注入
7. 新增 `switch_persona(&mut self, id: &str)` → 切换人格（路由到新金字塔）
8. 新增 `active_persona(&self) -> &Persona` → 暴露当前人格信息给 Orchestrator
9. 删除旧的 `gather_all_candidates` 逻辑

**兼容性:** `base_dir()` 和 `stats()` 方法保留，适配新结构。

**提交:** `refactor(memory): MemoryBrain 重构为金字塔+人格架构`

---

### Task 14: Orchestrator 集成

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`

**改动点:**

1. **创建 MemoryBrain 时传入 PersonaManager**: 不再硬编码 persona_id，由 PersonaManager 管理
2. **启动注入**: `inject_memory_context` 改为调用 `auto_inject()` + 人格 prompt 注入
3. **评估脑接口适配**: 评估信息从 `eval-info.json` 加载，敏感度从 PersonaConfig 读取
4. **人格切换**: 新增 `switch_persona` 方法，通知主脑更新 prompt，通知记忆脑切换金字塔
5. **pending_analysis 路径**: 适配 per-persona 目录
6. **配置支持**: analysis_interval 从 PersonaConfig 读取（替代硬编码 5）

**行号参考:**
- 行 1830: `create_sub_brains()` → 传入 persona_id
- 行 479: 启动注入 → 改用 `auto_inject()`
- 行 742: `store_turns` → 不变（内部已适配）
- 行 1340: `tick_and_should_analyze` → interval 可配
- 行 779: 评估信息加载 → 改用 `eval-info.json`

**提交:** `feat(orchestrator): 集成金字塔记忆脑 + 人格系统`

---

### Task 15: 旧数据迁移

**Files:**
- Create: `rust/crates/brain-migration/src/main.rs`

**设计要点:** 将现有 `~/.ai-brain/` 下的旧格式数据迁移到新的金字塔目录结构。

**迁移策略:**

1. L1: `sessions/*.jsonl` → `personas/default/pyramid/l1-raw/*.jsonl`（直接移动）
2. L2: `memory/summaries/*.json` → 保留不动，下次四步分析时自动生成 L2
3. L3/L4: 重新生成（不做迁移，等下次分析时 LLM 自动浓缩）
4. Profile: `memory/profile/user_profile.json` → 重新生成（100字上限）
5. EvalInfo: `memory/pitfall/` + `memory/evolution/` + `memory/eval-requirement/` → 合并为 `eval-info.json`

**提交:** `feat(migration): 旧数据迁移工具`

---

### Task 16: 清理旧代码

**Files:**
- Delete: `brain-memory/src/short_term.rs`
- Delete: `brain-memory/src/event_index.rs`
- Delete: `brain-memory/src/task_summary.rs`
- Delete: `brain-memory/src/index_layer.rs`
- Delete: `brain-memory/src/consolidation.rs`
- Delete: `brain-memory/src/guardian.rs`
- Delete: `brain-memory/src/memory_iteration.rs`
- Modify: `brain-memory/src/lib.rs` — 移除旧模块导出

**前提:** Task 13 完成且所有测试通过后执行。

**提交:** `chore(memory): 清理旧记忆系统代码`

---

## 验证检查清单

每个 Task 完成后:
- [ ] `cargo test -p brain-memory` 全部通过
- [ ] `cargo clippy -p brain-memory -- -D warnings` 无警告
- [ ] `cargo fmt` 格式正确
- [ ] 设计文档与实现一致

全部 Task 完成后:
- [ ] `cargo test --workspace` 全部通过
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` 无警告
- [ ] 手动启动智脑，验证记忆注入和召回正常
- [ ] 旧数据迁移后验证数据完整性
