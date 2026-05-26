//! 金字塔层级类型定义
//!
//! 四层金字塔: L1(全量基座) → L2(任务摘要池) → L3(经验抽象层) → L4(潜意识层)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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

// --- L1 类型 ---

/// L1 引用（指向原始记忆的具体段落）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L1Ref {
    pub session: String,
    /// 段落索引（0-based）
    pub paragraphs: Vec<usize>,
}

// --- L2 类型 ---

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
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// L2 索引条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryIndexEntry {
    pub task_id: String,
    pub task_type: TaskType,
    pub task_name: String,
    pub tags: Vec<String>,
    pub importance: f64,
}

/// L2 索引（任务→L1 映射）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryIndex {
    pub entries: Vec<SummaryIndexEntry>,
    pub updated_at: DateTime<Utc>,
}

// --- L3 类型 ---

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

/// 关键词索引条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeywordIndex {
    pub keyword: String,
    pub l2_task_ids: Vec<String>,
}

/// L3 类型化经验文件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeExperience {
    pub task_type: TaskType,
    pub experiences: Vec<Experience>,
    pub l2_refs: Vec<String>,
    /// 该类型内的关键词→L2任务映射索引
    pub index: Vec<KeywordIndex>,
    pub updated_at: DateTime<Utc>,
}

/// L3 索引条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbstractIndexEntry {
    pub task_type: TaskType,
    pub experience_count: usize,
    pub injectable_count: usize,
    pub l2_task_count: usize,
}

/// L3 索引（类型→L2 映射）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbstractIndex {
    pub entries: Vec<AbstractIndexEntry>,
    pub updated_at: DateTime<Utc>,
}

// --- L4 类型 ---

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
    #[serde(default = "chrono::Utc::now")]
    pub updated_at: DateTime<Utc>,
}

// --- Profile + EvalInfo ---

/// 用户画像（100字上限）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaProfile {
    pub summary: String,
    pub updated_at: DateTime<Utc>,
}

/// 评估脑信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalInfo {
    pub requirements: Vec<String>,
    pub pitfalls: Vec<String>,
    pub rules: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pyramid_layer_serde_roundtrip() {
        for layer in [
            PyramidLayer::Raw,
            PyramidLayer::Summary,
            PyramidLayer::Abstract,
            PyramidLayer::Subconscious,
        ] {
            let json = serde_json::to_string(&layer).unwrap();
            let back: PyramidLayer = serde_json::from_str(&json).unwrap();
            assert_eq!(layer, back);
        }
    }

    #[test]
    fn task_type_serde_roundtrip() {
        let cases = [
            TaskType::Coding,
            TaskType::Writing,
            TaskType::Troubleshooting,
            TaskType::Research,
            TaskType::Multimedia,
            TaskType::Configuration,
            TaskType::Other("design".into()),
        ];
        for t in cases {
            let json = serde_json::to_string(&t).unwrap();
            let back: TaskType = serde_json::from_str(&json).unwrap();
            assert_eq!(t, back);
        }
    }

    #[test]
    fn l1_ref_serde() {
        let r = L1Ref {
            session: "sess-001".into(),
            paragraphs: vec![0, 3, 5],
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: L1Ref = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session, "sess-001");
        assert_eq!(back.paragraphs, vec![0, 3, 5]);
    }

    #[test]
    fn task_summary_full_roundtrip() {
        let now = Utc::now();
        let ts = TaskSummary {
            task_id: "task-001".into(),
            task_type: TaskType::Coding,
            task_name: "TUI鼠标修复".into(),
            summary: "修复鼠标捕获问题".into(),
            l1_refs: vec![L1Ref {
                session: "sess-001".into(),
                paragraphs: vec![3, 4],
            }],
            tags: vec!["TUI".into(), "鼠标".into()],
            importance: 0.85,
            created_at: now,
            updated_at: now,
        };
        let json = serde_json::to_string_pretty(&ts).unwrap();
        let back: TaskSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back.task_id, "task-001");
        assert_eq!(back.task_type, TaskType::Coding);
        assert_eq!(back.tags.len(), 2);
        assert!((back.importance - 0.85).abs() < f64::EPSILON);
    }

    #[test]
    fn subconscious_data_structure() {
        let data = SubconsciousData {
            triggers: vec![SubconsciousTrigger {
                keyword: "红冲逻辑".into(),
                l3_type: TaskType::Coding,
                l2_task: "task-033".into(),
            }],
            narrative: "用户是Rust全栈开发者".into(),
            version: 42,
            updated_at: Utc::now(),
        };
        assert!(data.narrative.len() <= 500);
        assert_eq!(data.triggers.len(), 1);
        assert_eq!(data.triggers[0].keyword, "红冲逻辑");
    }

    #[test]
    fn experience_injectable_flag() {
        let exp = Experience {
            pattern: "工具替代".into(),
            description: "WebSearch失败用curl替代".into(),
            source_tasks: vec!["task-001".into()],
            frequency: 3,
            injectable: true,
        };
        let json = serde_json::to_string(&exp).unwrap();
        let back: Experience = serde_json::from_str(&json).unwrap();
        assert!(back.injectable);
        assert_eq!(back.frequency, 3);
    }

    #[test]
    fn type_experience_with_index() {
        let te = TypeExperience {
            task_type: TaskType::Coding,
            experiences: vec![Experience {
                pattern: "测试".into(),
                description: "先写测试".into(),
                source_tasks: vec!["t-1".into()],
                frequency: 2,
                injectable: false,
            }],
            l2_refs: vec!["t-1".into()],
            index: vec![KeywordIndex {
                keyword: "TUI".into(),
                l2_task_ids: vec!["t-1".into()],
            }],
            updated_at: Utc::now(),
        };
        let json = serde_json::to_string(&te).unwrap();
        let back: TypeExperience = serde_json::from_str(&json).unwrap();
        assert_eq!(back.experiences.len(), 1);
        assert_eq!(back.index[0].keyword, "TUI");
    }

    #[test]
    fn eval_info_structure() {
        let info = EvalInfo {
            requirements: vec!["不能重复".into()],
            pitfalls: vec!["避免空指针".into()],
            rules: vec!["先测试再提交".into()],
            updated_at: Utc::now(),
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: EvalInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.requirements.len(), 1);
        assert_eq!(back.pitfalls.len(), 1);
        assert_eq!(back.rules.len(), 1);
    }

    #[test]
    fn summary_index_entries() {
        let idx = SummaryIndex {
            entries: vec![
                SummaryIndexEntry {
                    task_id: "t-1".into(),
                    task_type: TaskType::Coding,
                    task_name: "A".into(),
                    tags: vec!["x".into()],
                    importance: 0.9,
                },
                SummaryIndexEntry {
                    task_id: "t-2".into(),
                    task_type: TaskType::Writing,
                    task_name: "B".into(),
                    tags: vec![],
                    importance: 0.5,
                },
            ],
            updated_at: Utc::now(),
        };
        assert_eq!(idx.entries.len(), 2);
        assert_eq!(idx.entries[0].task_id, "t-1");
    }

    #[test]
    fn abstract_index_entries() {
        let idx = AbstractIndex {
            entries: vec![AbstractIndexEntry {
                task_type: TaskType::Coding,
                experience_count: 5,
                injectable_count: 2,
                l2_task_count: 3,
            }],
            updated_at: Utc::now(),
        };
        assert_eq!(idx.entries[0].experience_count, 5);
        assert_eq!(idx.entries[0].injectable_count, 2);
    }
}
