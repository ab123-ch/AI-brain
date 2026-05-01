use crate::error::{EvolverError, Result};
use brain_llm::LlmProvider;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub category: FindingCategory,
    pub description: String,
    pub file_path: Option<String>,
    pub severity: Severity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FindingCategory {
    Performance,
    CodeQuality,
    MissingFeature,
    Architecture,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Severity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionSuggestion {
    pub title: String,
    pub description: String,
    pub reference: Option<String>,
    pub priority: Priority,
    pub target_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Priority {
    Low,
    Medium,
    High,
}

pub struct IdleScanner {
    #[allow(dead_code)] // 后续 LLM 驱动研究使用
    llm: Arc<dyn LlmProvider>,
    idle_threshold: Duration,
    last_activity: Instant,
    suggestion_store: std::path::PathBuf,
}

impl IdleScanner {
    pub fn new(llm: Arc<dyn LlmProvider>, suggestion_store: &std::path::Path) -> Self {
        Self {
            llm,
            idle_threshold: Duration::from_secs(2 * 3600), // 2 小时
            last_activity: Instant::now(),
            suggestion_store: suggestion_store.to_path_buf(),
        }
    }

    pub fn is_idle(&self) -> bool {
        self.last_activity.elapsed() >= self.idle_threshold
    }

    pub fn touch_activity(&mut self) {
        self.last_activity = Instant::now();
    }

    /// 保存建议到文件
    pub async fn save_suggestions(&self, suggestions: &[EvolutionSuggestion]) -> Result<()> {
        tokio::fs::create_dir_all(&self.suggestion_store).await?;
        let json = serde_json::to_string_pretty(suggestions)
            .map_err(|e| EvolverError::Sandbox(format!("序列化失败: {e}")))?;
        let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
        let path = self.suggestion_store.join(format!("suggestions-{timestamp}.json"));
        tokio::fs::write(&path, json).await?;
        Ok(())
    }
}
