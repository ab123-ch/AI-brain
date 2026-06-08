//! MemoryAccess — 进化脑记忆访问接口
//!
//! 提供渐进式召回和记忆写入的抽象接口，
//! 让 CycleRunner 可以调用记忆脑功能而不直接依赖复杂架构。

use crate::error::Result;

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
        Self { preset_recall: preset }
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
}