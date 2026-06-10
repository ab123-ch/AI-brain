//! MemoryAccess — 进化脑记忆访问接口
//!
//! 提供渐进式召回和记忆写入的抽象接口，
//! 让 CycleRunner 可以调用记忆脑功能而不直接依赖复杂架构。

use crate::error::{EvolverError, Result};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// 记忆召回结果
#[derive(Clone, Debug)]
pub struct RecallResult {
    /// L4 触发词匹配
    pub trigger_matches: Vec<String>,
    /// L3 经验摘要
    pub experience_summary: Option<String>,
    /// L2 任务摘要（上次学习进度）
    pub task_summary: Option<String>,
    /// 相关的 pitfall/踩坑记录
    pub related_pitfalls: Vec<String>,
}

/// 记忆写入请求
#[derive(Clone, Debug)]
pub struct MemoryWriteRequest {
    /// 写入层级
    pub layer: MemoryLayer,
    /// 内容
    pub content: String,
    /// 来源标识
    pub source: String,
}

/// 记忆层级
#[derive(Clone, Debug, PartialEq)]
pub enum MemoryLayer {
    /// L1 原始记忆
    Raw,
    /// L2 任务摘要
    Summary,
    /// L3 经验抽象
    Abstract,
    /// L4 触发词
    Subconscious,
}

/// 进化脑记忆访问接口
///
/// 实现可以是：
/// - 真实的 PyramidStorage + ProgressiveRecall
/// - Mock（用于测试）
/// - Stub（用于早期开发）
pub trait MemoryAccess: Send + Sync {
    /// 渐进式召回（自顶向下 L4→L3→L2→L1）
    ///
    /// `query`: 查询关键词（如目标描述、技能领域）
    /// 返回：匹配的触发词、经验、任务摘要、相关 pitfall
    fn progressive_recall(&self, query: &str) -> Result<RecallResult>;

    /// 写入记忆（研究结果、学习笔记、经验抽象）
    fn write_memory(&self, request: MemoryWriteRequest) -> Result<()>;

    /// 批量写入（Phase 2 研究结果全量写入 L1）
    fn batch_write(&self, requests: Vec<MemoryWriteRequest>) -> Result<()> {
        for req in requests {
            self.write_memory(req)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Mock 实现（用于测试）
// ---------------------------------------------------------------------------

/// Mock MemoryAccess — 返回预设数据
pub struct MockMemoryAccess {
    preset_recall: RecallResult,
}

impl MockMemoryAccess {
    pub fn new(preset: RecallResult) -> Self {
        Self {
            preset_recall: preset,
        }
    }

    pub fn empty() -> Self {
        Self {
            preset_recall: RecallResult {
                trigger_matches: vec![],
                experience_summary: None,
                task_summary: None,
                related_pitfalls: vec![],
            },
        }
    }

    pub fn with_progress(summary: String) -> Self {
        Self {
            preset_recall: RecallResult {
                trigger_matches: vec!["async".into()],
                experience_summary: Some("已掌握 Pin 语义".into()),
                task_summary: Some(summary),
                related_pitfalls: vec!["async runtime 模型理解不足".into()],
            },
        }
    }
}

impl MemoryAccess for MockMemoryAccess {
    fn progressive_recall(&self, _query: &str) -> Result<RecallResult> {
        Ok(self.preset_recall.clone())
    }

    fn write_memory(&self, _request: MemoryWriteRequest) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Stub 实现（早期开发用）
// ---------------------------------------------------------------------------

/// Stub MemoryAccess — 暂时返回空结果，不实际写入
pub struct StubMemoryAccess;

impl MemoryAccess for StubMemoryAccess {
    fn progressive_recall(&self, _query: &str) -> Result<RecallResult> {
        Ok(RecallResult {
            trigger_matches: vec![],
            experience_summary: None,
            task_summary: None,
            related_pitfalls: vec![],
        })
    }

    fn write_memory(&self, _request: MemoryWriteRequest) -> Result<()> {
        // Stub: 暂不实现写入
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PyramidMemoryAccess — 真实金字塔文件系统实现
// ---------------------------------------------------------------------------

/// 基于文件系统的金字塔记忆访问
///
/// 直接读写金字塔目录结构，不依赖 brain-memory crate 的复杂类型，
/// 使用 std::fs 同步 I/O（MemoryAccess trait 是同步的）。
pub struct PyramidMemoryAccess {
    pyramid_root: PathBuf,
}

impl PyramidMemoryAccess {
    pub fn new(pyramid_root: PathBuf) -> Self {
        Self { pyramid_root }
    }

    fn l1_dir(&self) -> PathBuf {
        self.pyramid_root.join("l1-raw")
    }

    fn l2_dir(&self) -> PathBuf {
        self.pyramid_root.join("l2-summary")
    }

    fn l3_dir(&self) -> PathBuf {
        self.pyramid_root.join("l3-abstract")
    }

    fn l4_path(&self) -> PathBuf {
        self.pyramid_root.join("l4-subconscious.json")
    }

    fn eval_info_path(&self) -> PathBuf {
        self.pyramid_root
            .parent()
            .map(|p| p.join("eval-info.json"))
            .unwrap_or_else(|| self.pyramid_root.join("eval-info.json"))
    }
}

impl MemoryAccess for PyramidMemoryAccess {
    fn progressive_recall(&self, query: &str) -> Result<RecallResult> {
        let query_lower = query.to_lowercase();
        let mut result = RecallResult {
            trigger_matches: vec![],
            experience_summary: None,
            task_summary: None,
            related_pitfalls: vec![],
        };

        // L4: 读取潜意识触发词，按关键词过滤
        if let Ok(raw) = fs::read_to_string(self.l4_path()) {
            if let Ok(data) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(triggers) = data.get("triggers").and_then(|t| t.as_array()) {
                    for trigger in triggers {
                        if let Some(keyword) = trigger.get("keyword").and_then(|k| k.as_str()) {
                            if query_lower.contains(&keyword.to_lowercase()) {
                                result.trigger_matches.push(keyword.to_string());
                            }
                        }
                    }
                }
            }
        }

        // L2: 读取摘要文件，按关键词匹配
        if let Ok(entries) = fs::read_dir(self.l2_dir()) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json")
                    && path.file_name() != Some(std::ffi::OsStr::new("index.json"))
                {
                    if let Ok(content) = fs::read_to_string(&path) {
                        if content.to_lowercase().contains(&query_lower)
                            && result.task_summary.is_none()
                        {
                            // 取第一个匹配的摘要
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                                if let Some(name) = val.get("task_name").and_then(|v| v.as_str()) {
                                    result.task_summary = Some(name.to_string());
                                } else if let Some(sum) =
                                    val.get("summary").and_then(|v| v.as_str())
                                {
                                    result.task_summary = Some(sum.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        // L3: 读取经验文件
        if let Ok(entries) = fs::read_dir(self.l3_dir()) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json")
                    && path.file_name() != Some(std::ffi::OsStr::new("index.json"))
                {
                    if let Ok(content) = fs::read_to_string(&path) {
                        if content.to_lowercase().contains(&query_lower)
                            && result.experience_summary.is_none()
                        {
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                                if let Some(exps) =
                                    val.get("experiences").and_then(|v| v.as_array())
                                {
                                    let descriptions: Vec<String> = exps
                                        .iter()
                                        .filter_map(|e| {
                                            e.get("description")
                                                .and_then(|d| d.as_str())
                                                .map(String::from)
                                        })
                                        .collect();
                                    if !descriptions.is_empty() {
                                        result.experience_summary = Some(descriptions.join("; "));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Pitfalls: 读取 eval-info.json
        if let Ok(raw) = fs::read_to_string(self.eval_info_path()) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(pitfalls) = val.get("pitfalls").and_then(|v| v.as_array()) {
                    for p in pitfalls {
                        if let Some(s) = p.as_str() {
                            if s.to_lowercase().contains(&query_lower) || query_lower.len() <= 3 {
                                result.related_pitfalls.push(s.to_string());
                            }
                        }
                    }
                }
            }
        }

        Ok(result)
    }

    fn write_memory(&self, request: MemoryWriteRequest) -> Result<()> {
        match request.layer {
            MemoryLayer::Raw => {
                fs::create_dir_all(self.l1_dir())
                    .map_err(|e| EvolverError::Sandbox(e.to_string()))?;
                let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S%.f");
                let path = self.l1_dir().join(format!("evolver-{timestamp}.jsonl"));
                let entry = serde_json::json!({
                    "role": "Evolver",
                    "content": request.content,
                    "source": request.source,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                });
                let mut line = serde_json::to_string(&entry)
                    .map_err(|e| EvolverError::Sandbox(format!("序列化失败: {e}")))?;
                line.push('\n');
                let mut f = fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|e| EvolverError::Sandbox(e.to_string()))?;
                f.write_all(line.as_bytes())
                    .map_err(|e| EvolverError::Sandbox(e.to_string()))?;
            }
            MemoryLayer::Summary => {
                fs::create_dir_all(self.l2_dir())
                    .map_err(|e| EvolverError::Sandbox(e.to_string()))?;
                let path = self.l2_dir().join(format!("{}.json", request.source));
                let entry = serde_json::json!({
                    "summary": request.content,
                    "source": request.source,
                    "created_at": chrono::Utc::now().to_rfc3339(),
                });
                let data = serde_json::to_string_pretty(&entry)
                    .map_err(|e| EvolverError::Sandbox(format!("序列化失败: {e}")))?;
                fs::write(&path, data).map_err(|e| EvolverError::Sandbox(e.to_string()))?;
            }
            MemoryLayer::Abstract => {
                fs::create_dir_all(self.l3_dir())
                    .map_err(|e| EvolverError::Sandbox(e.to_string()))?;
                let path = self.l3_dir().join("evolver.json");
                let entry = serde_json::json!({
                    "summary": request.content,
                    "source": request.source,
                    "created_at": chrono::Utc::now().to_rfc3339(),
                });
                let data = serde_json::to_string_pretty(&entry)
                    .map_err(|e| EvolverError::Sandbox(format!("序列化失败: {e}")))?;
                fs::write(&path, data).map_err(|e| EvolverError::Sandbox(e.to_string()))?;
            }
            MemoryLayer::Subconscious => {
                // 读改写 l4-subconscious.json，追加 trigger 关键词
                let path = self.l4_path();
                fs::create_dir_all(path.parent().unwrap_or(&self.pyramid_root))
                    .map_err(|e| EvolverError::Sandbox(e.to_string()))?;
                let mut data: serde_json::Value = if path.exists() {
                    let raw = fs::read_to_string(&path)
                        .map_err(|e| EvolverError::Sandbox(e.to_string()))?;
                    serde_json::from_str(&raw).unwrap_or(serde_json::json!({}))
                } else {
                    serde_json::json!({"triggers": [], "narrative": "", "version": 1})
                };
                if let Some(triggers) = data.get_mut("triggers").and_then(|t| t.as_array_mut()) {
                    let new_trigger = serde_json::json!({
                        "keyword": request.content,
                        "l3_type": "Coding",
                        "l2_task": request.source,
                    });
                    triggers.push(new_trigger);
                }
                let out = serde_json::to_string_pretty(&data)
                    .map_err(|e| EvolverError::Sandbox(format!("序列化失败: {e}")))?;
                fs::write(&path, out).map_err(|e| EvolverError::Sandbox(e.to_string()))?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_memory_access_empty() {
        let mock = MockMemoryAccess::empty();
        let result = mock.progressive_recall("test").unwrap();
        assert!(result.trigger_matches.is_empty());
        assert!(result.experience_summary.is_none());
    }

    #[test]
    fn test_mock_memory_access_with_progress() {
        let mock = MockMemoryAccess::with_progress("上次学到了 X".into());
        let result = mock.progressive_recall("async").unwrap();
        assert_eq!(result.trigger_matches.len(), 1);
        assert!(result.task_summary.is_some());
    }

    #[test]
    fn test_stub_memory_access() {
        let stub = StubMemoryAccess;
        let result = stub.progressive_recall("test").unwrap();
        assert!(result.trigger_matches.is_empty());

        // Write should succeed (stub)
        let req = MemoryWriteRequest {
            layer: MemoryLayer::Raw,
            content: "test".into(),
            source: "test".into(),
        };
        assert!(stub.write_memory(req).is_ok());
    }

    #[test]
    fn test_batch_write() {
        let mock = MockMemoryAccess::empty();
        let requests = vec![
            MemoryWriteRequest {
                layer: MemoryLayer::Raw,
                content: "raw content".into(),
                source: "research".into(),
            },
            MemoryWriteRequest {
                layer: MemoryLayer::Summary,
                content: "summary".into(),
                source: "learn".into(),
            },
        ];
        assert!(mock.batch_write(requests).is_ok());
    }

    // --- PyramidMemoryAccess tests ---

    fn setup_pyramid(tmp: &tempfile::TempDir) -> PyramidMemoryAccess {
        let root = tmp.path().join("personas").join("test").join("pyramid");
        fs::create_dir_all(root.join("l1-raw")).unwrap();
        fs::create_dir_all(root.join("l2-summary")).unwrap();
        fs::create_dir_all(root.join("l3-abstract")).unwrap();
        PyramidMemoryAccess::new(root)
    }

    #[test]
    fn test_pyramid_recall_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let access = setup_pyramid(&tmp);
        let result = access.progressive_recall("anything").unwrap();
        assert!(result.trigger_matches.is_empty());
        assert!(result.experience_summary.is_none());
    }

    #[test]
    fn test_pyramid_write_raw() {
        let tmp = tempfile::tempdir().unwrap();
        let access = setup_pyramid(&tmp);
        let req = MemoryWriteRequest {
            layer: MemoryLayer::Raw,
            content: "research finding".into(),
            source: "evolver-test".into(),
        };
        access.write_memory(req).unwrap();
        // Verify file was created in l1-raw
        let l1_dir = access.l1_dir();
        let files: Vec<_> = fs::read_dir(&l1_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(files.len(), 1);
        assert!(files[0].path().to_str().unwrap().contains("evolver-"));
        assert!(files[0].path().extension().unwrap() == "jsonl");
    }

    #[test]
    fn test_pyramid_write_summary() {
        let tmp = tempfile::tempdir().unwrap();
        let access = setup_pyramid(&tmp);
        let req = MemoryWriteRequest {
            layer: MemoryLayer::Summary,
            content: "learned async patterns".into(),
            source: "task-001".into(),
        };
        access.write_memory(req).unwrap();
        let path = access.l2_dir().join("task-001.json");
        assert!(path.exists());
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("learned async patterns"));
    }

    #[test]
    fn test_pyramid_write_subconscious_and_recall() {
        let tmp = tempfile::tempdir().unwrap();
        let access = setup_pyramid(&tmp);

        // Write a trigger
        let req = MemoryWriteRequest {
            layer: MemoryLayer::Subconscious,
            content: "TUI layout".into(),
            source: "evolver-cycle".into(),
        };
        access.write_memory(req).unwrap();

        // Recall with matching query
        let result = access.progressive_recall("TUI layout fix").unwrap();
        assert!(result.trigger_matches.contains(&"TUI layout".to_string()));

        // Recall with non-matching query
        let result2 = access.progressive_recall("database query").unwrap();
        assert!(result2.trigger_matches.is_empty());
    }

    #[test]
    fn test_pyramid_recall_from_l2() {
        let tmp = tempfile::tempdir().unwrap();
        let access = setup_pyramid(&tmp);

        // Write L2 summary
        let req = MemoryWriteRequest {
            layer: MemoryLayer::Summary,
            content: "async runtime patterns".into(),
            source: "task-async".into(),
        };
        access.write_memory(req).unwrap();

        // Recall matching
        let result = access.progressive_recall("async runtime").unwrap();
        assert!(result.task_summary.is_some());
    }

    #[test]
    fn test_pyramid_recall_eval_info_pitfalls() {
        let tmp = tempfile::tempdir().unwrap();
        let access = setup_pyramid(&tmp);

        // Write eval-info.json in parent/parent directory
        let persona_dir = tmp.path().join("personas").join("test");
        fs::create_dir_all(&persona_dir).unwrap();
        let eval_path = persona_dir.join("eval-info.json");
        let eval_data = serde_json::json!({
            "requirements": [],
            "pitfalls": ["avoid deadlock in async", "check for None"],
            "rules": [],
            "updated_at": "2026-06-10T00:00:00Z"
        });
        fs::write(
            &eval_path,
            serde_json::to_string_pretty(&eval_data).unwrap(),
        )
        .unwrap();

        let result = access.progressive_recall("deadlock").unwrap();
        assert!(result
            .related_pitfalls
            .iter()
            .any(|p| p.contains("deadlock")));
    }
}
