# 记忆金字塔重构实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 将 brain-memory crate 从追加式存储重构为四人隔离的金字塔提炼式记忆系统。

**Architecture:** 四层金字塔(L1全量基座→L2任务摘要池→L3经验抽象层→L4潜意识触发词)，每层全量重生成而非追加。多人格完全隔离，每人格独立目录。渐进式召回(LLM驱动，自顶向下)。

**Tech Stack:** Rust, serde_json, chrono, tokio(async), brain-llm(LLM trait)

**设计文档:** `docs/plans/2026-05-22-memory-pyramid-redesign.md`

---

## 依赖关系图

```
Task 1 (类型定义)
  ├─→ Task 2 (per-persona 存储)
  │     ├─→ Task 3 (L1 Raw Pool)
  │     ├─→ Task 4 (L2 Summary Pool)
  │     ├─→ Task 5 (L3 Abstraction Layer)
  │     ├─→ Task 6 (L4 Subconscious)
  │     └─→ Task 7 (Profile + EvalInfo)
  ├─→ Task 8 (Prompt 重写)
  │     └─→ Task 9 (四步浓缩引擎)
  ├─→ Task 10 (渐进式召回引擎)
  └─→ Task 11 (MemoryBrain 重构)
        └─→ Task 12 (Orchestrator 集成)
              └─→ Task 13 (旧数据迁移)
```

---

### Task 1: 核心类型定义

**Files:**
- Create: `rust/crates/brain-memory/src/pyramid_types.rs`
- Test: `rust/crates/brain-memory/src/pyramid_types.rs` (inline tests)

**设计要点:** 定义金字塔所有层级的数据结构，这些类型是整个重构的基础。

**Step 1: 定义金字塔层级枚举和核心类型**

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

### Task 2: Per-Persona 存储层

**Files:**
- Create: `rust/crates/brain-memory/src/pyramid_storage.rs`
- Modify: `rust/crates/brain-memory/src/lib.rs`

**设计要点:** 封装 per-persona 目录结构的创建和文件读写。所有人格数据存储在 `~/.ai-brain/personas/{persona_id}/pyramid/` 下。

**Step 1: 写测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pyramid_dirs_created() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PyramidStorage::new(tmp.path().to_path_buf(), "cyber-brain");
        store.ensure_dirs().unwrap();
        assert!(store.l1_dir().exists());
        assert!(store.l2_dir().exists());
        assert!(store.l3_dir().exists());
        assert!(store.l4_path().parent().unwrap().exists());
    }

    #[test]
    fn default_persona_uses_base_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PyramidStorage::new(tmp.path().to_path_buf(), "");
        assert_eq!(store.pyramid_dir(), tmp.path().join("pyramid"));
    }

    #[test]
    fn named_persona_creates_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let store = PyramidStorage::new(tmp.path().to_path_buf(), "writer");
        assert_eq!(
            store.pyramid_dir(),
            tmp.path().join("personas").join("writer").join("pyramid")
        );
    }
}
```

**Step 2: 实现 PyramidStorage**

```rust
// pyramid_storage.rs

use crate::error::MemoryError;
use serde::de::DeserializeOwned;
use std::path::PathBuf;

pub struct PyramidStorage {
    base_dir: PathBuf,
    persona_id: String,
}

impl PyramidStorage {
    pub fn new(base_dir: PathBuf, persona_id: &str) -> Self {
        Self {
            base_dir,
            persona_id: persona_id.to_string(),
        }
    }

    /// 金字塔根目录（per-persona）
    pub fn pyramid_dir(&self) -> PathBuf {
        if self.persona_id.is_empty() {
            self.base_dir.join("pyramid")
        } else {
            self.base_dir.join("personas").join(&self.persona_id).join("pyramid")
        }
    }

    pub fn l1_dir(&self) -> PathBuf { self.pyramid_dir().join("l1-raw") }
    pub fn l2_dir(&self) -> PathBuf { self.pyramid_dir().join("l2-summary") }
    pub fn l3_dir(&self) -> PathBuf { self.pyramid_dir().join("l3-abstract") }
    pub fn l4_path(&self) -> PathBuf { self.pyramid_dir().join("l4-subconscious.json") }
    pub fn profile_path(&self) -> PathBuf {
        if self.persona_id.is_empty() {
            self.base_dir.join("pyramid").join("profile.json")
        } else {
            self.base_dir.join("personas").join(&self.persona_id).join("profile.json")
        }
    }
    pub fn eval_info_path(&self) -> PathBuf {
        if self.persona_id.is_empty() {
            self.base_dir.join("pyramid").join("eval-info.json")
        } else {
            self.base_dir.join("personas").join(&self.persona_id).join("eval-info.json")
        }
    }

    pub fn ensure_dirs(&self) -> Result<(), MemoryError> {
        for dir in &[self.l1_dir(), self.l2_dir(), self.l3_dir()] {
            std::fs::create_dir_all(dir)
                .map_err(|e| MemoryError::Io(e))?;
        }
        if let Some(parent) = self.l4_path().parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| MemoryError::Io(e))?;
        }
        Ok(())
    }

    pub fn read_json<T: DeserializeOwned>(&self, path: &PathBuf) -> Result<Option<T>, MemoryError> {
        if !path.exists() { return Ok(None); }
        let data = std::fs::read_to_string(path)
            .map_err(MemoryError::Io)?;
        let val: T = serde_json::from_str(&data)
            .map_err(MemoryError::Serde)?;
        Ok(Some(val))
    }

    pub fn write_json<T: Serialize>(&self, path: &PathBuf, data: &T) -> Result<(), MemoryError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| MemoryError::Io(e))?;
        }
        let json = serde_json::to_string_pretty(data)
            .map_err(MemoryError::Serde)?;
        std::fs::write(path, json)
            .map_err(MemoryError::Io)?;
        Ok(())
    }
}
```

**Step 3: 运行测试并提交**

```bash
cargo test -p brain-memory pyramid_storage
git add -A && git commit -m "feat(memory): per-persona 金字塔存储层"
```

---

### Task 3: L1 Raw Pool（适配现有 raw_layer.rs）

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

### Task 4: L2 Summary Pool（任务分类摘要）

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

### Task 5: L3 Abstraction Layer（经验抽象层）

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

### Task 6: L4 Subconscious（增强版潜意识层）

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

### Task 7: Profile + EvalInfo（融合知识层）

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

### Task 8: Prompt 重写

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

### Task 9: 四步浓缩引擎

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

### Task 10: 渐进式召回引擎

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

### Task 11: MemoryBrain 重构

**Files:**
- Modify: `rust/crates/brain-memory/src/memory_brain.rs`

**设计要点:** 用新的金字塔组件替代旧的扁平存储。保持对外接口不变（`store_turns`, `recall_for_context` 等），内部路由到金字塔。

**关键改动:**

1. `MemoryBrainConfig` 新增 `persona_id: String` 和 `analysis_interval: u32`
2. 内部持有 `PyramidStorage` 而非多个独立 `Storage`
3. `store_turns` → 写入 L1（不再写 L2 短期记忆）
4. `tick_and_should_analyze` → 触发 `ConcentrationEngine::run`
5. `recall_for_context` → 委托 `ProgressiveRecall::recall`
6. `load_subconscious_summary` → 从 L4 读取
7. 删除旧的 `gather_all_candidates` 逻辑

**兼容性:** `base_dir()` 和 `stats()` 方法保留，适配新结构。

**提交:** `refactor(memory): MemoryBrain 重构为金字塔架构`

---

### Task 12: Orchestrator 集成

**Files:**
- Modify: `rust/crates/ai-brain-cli/src/orchestrator.rs`

**改动点:**

1. **创建 MemoryBrain 时传入 persona_id**: 从配置或环境变量读取当前人格
2. **启动注入**: `inject_memory_context` 改为调用 `auto_inject()`
3. **评估脑接口适配**: 评估信息从 `eval-info.json` 加载，而非从旧 Store 加载
4. **配置支持**: `analysis_interval` 从 `config.toml` 读取
5. **pending_analysis 路径**: 适配 per-persona 目录

**行号参考:**
- 行 1830: `create_sub_brains()` → 传入 persona_id
- 行 479: 启动注入 → 改用 `auto_inject()`
- 行 742: `store_turns` → 不变（内部已适配）
- 行 1340: `tick_and_should_analyze` → interval 可配
- 行 779: 评估信息加载 → 改用 `eval-info.json`

**提交:** `feat(orchestrator): 集成金字塔记忆脑`

---

### Task 13: 旧数据迁移

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

### Task 14: 清理旧代码

**Files:**
- Delete: `brain-memory/src/short_term.rs`
- Delete: `brain-memory/src/event_index.rs`
- Delete: `brain-memory/src/task_summary.rs`
- Delete: `brain-memory/src/index_layer.rs`
- Delete: `brain-memory/src/consolidation.rs`
- Delete: `brain-memory/src/guardian.rs`
- Delete: `brain-memory/src/memory_iteration.rs`
- Modify: `brain-memory/src/lib.rs` — 移除旧模块导出

**前提:** Task 11 完成且所有测试通过后执行。

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
