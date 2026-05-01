use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::types::BrainId;

/// 顶层 Brain 配置
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BrainConfig {
    pub brain: BrainSection,
    pub python: PythonSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrainSection {
    /// 感知脑模型，默认 "haiku"
    pub model_fast: String,
    /// 慢思考模型，默认 "sonnet"
    pub model_slow: String,
    /// 记忆存储目录，默认 "~/.ai-brain/memory"
    pub memory_dir: PathBuf,
    /// 阈值配置
    pub thresholds: ThresholdConfig,
    /// 副脑初始权重
    pub weights: WeightConfig,
}

impl Default for BrainSection {
    fn default() -> Self {
        Self {
            model_fast: "haiku".into(),
            model_slow: "sonnet".into(),
            memory_dir: dirs_home().join(".ai-brain").join("memory"),
            thresholds: ThresholdConfig::default(),
            weights: WeightConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdConfig {
    /// 快思考置信度阈值，默认 0.7
    pub fast_think_confidence: f64,
    /// 巩固重要性阈值，默认 0.7
    pub consolidation_importance: f64,
    /// 召回最低 importance，默认 0.2
    pub memory_recall_min_importance: f64,
    /// 上下文使用率警告阈值，默认 0.60（触发 pending 替换）
    pub context_warning_threshold: f64,
    /// 上下文使用率危险阈值，默认 0.80（触发强制截断）
    pub context_danger_threshold: f64,
    /// 上下文窗口 token 上限，默认 1M（1_048_576）
    #[serde(default = "default_max_context_tokens")]
    pub max_context_tokens: u64,
}

fn default_max_context_tokens() -> u64 {
    1_048_576
}

impl Default for ThresholdConfig {
    fn default() -> Self {
        Self {
            fast_think_confidence: 0.7,
            consolidation_importance: 0.7,
            memory_recall_min_importance: 0.2,
            context_warning_threshold: 0.60,
            context_danger_threshold: 0.80,
            max_context_tokens: default_max_context_tokens(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightConfig {
    pub reasoning: f64,
    pub memory: f64,
    pub motor: f64,
    pub validation: f64,
    pub evolver: f64,
}

impl Default for WeightConfig {
    fn default() -> Self {
        Self {
            reasoning: 0.5,
            memory: 0.5,
            motor: 0.5,
            validation: 0.5,
            evolver: 0.3,
        }
    }
}

impl WeightConfig {
    #[must_use]
    pub fn to_map(&self) -> std::collections::HashMap<BrainId, crate::types::Weight> {
        use crate::types::Weight;
        let mut m = std::collections::HashMap::new();
        m.insert(BrainId::reasoning(), Weight(self.reasoning));
        m.insert(BrainId::memory(), Weight(self.memory));
        m.insert(BrainId::motor(), Weight(self.motor));
        m.insert(BrainId::validation(), Weight(self.validation));
        m.insert(BrainId::evolver(), Weight(self.evolver));
        m
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PythonSection {
    /// Python MCP Server 启动命令
    pub mcp_command: String,
    /// 启动参数
    pub mcp_args: Vec<String>,
}

impl Default for PythonSection {
    fn default() -> Self {
        Self {
            mcp_command: "python".into(),
            mcp_args: vec!["-m".into(), "ai_brain.server".into()],
        }
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_or_else(|_| PathBuf::from("/tmp"), PathBuf::from)
}
