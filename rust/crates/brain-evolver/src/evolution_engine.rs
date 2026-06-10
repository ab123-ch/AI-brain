use crate::error::{EvolverError, Result};
use crate::sandbox::Sandbox;
use crate::tdd_runner::EvolutionGoal;
use brain_llm::LlmProvider;
use std::path::Path;
use std::sync::Arc;

/// 进化状态
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvolutionStatus {
    Pending,
    Analyzing,
    WritingTests,
    Running,
    Testing,
    Regression,
    AwaitingApproval,
    Merging,
    Done,
    Failed(String),
    Discarded,
}

impl std::fmt::Display for EvolutionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "等待中"),
            Self::Analyzing => write!(f, "分析中"),
            Self::WritingTests => write!(f, "编写测试中"),
            Self::Running => write!(f, "执行中"),
            Self::Testing => write!(f, "测试中"),
            Self::Regression => write!(f, "回归测试中"),
            Self::AwaitingApproval => write!(f, "等待确认"),
            Self::Merging => write!(f, "合并中"),
            Self::Done => write!(f, "完成"),
            Self::Failed(e) => write!(f, "失败: {e}"),
            Self::Discarded => write!(f, "已丢弃"),
        }
    }
}

/// 进化引擎
pub struct EvolutionEngine {
    #[allow(dead_code)] // 后续 LLM 驱动进化循环使用
    llm: Arc<dyn LlmProvider>,
    repo_path: std::path::PathBuf,
    status: EvolutionStatus,
    current_sandbox: Option<Sandbox>,
    evolution_id: Option<String>,
}

impl EvolutionEngine {
    pub fn new(llm: Arc<dyn LlmProvider>, repo_path: &Path) -> Self {
        Self {
            llm,
            repo_path: repo_path.to_path_buf(),
            status: EvolutionStatus::Pending,
            current_sandbox: None,
            evolution_id: None,
        }
    }

    pub fn status(&self) -> &EvolutionStatus {
        &self.status
    }

    /// 是否有正在进行的进化任务
    pub fn is_busy(&self) -> bool {
        !matches!(
            self.status,
            EvolutionStatus::Pending
                | EvolutionStatus::Done
                | EvolutionStatus::Failed(_)
                | EvolutionStatus::Discarded
        )
    }

    /// 启动进化任务
    pub async fn start(&mut self, goal: EvolutionGoal) -> Result<()> {
        if self.is_busy() {
            return Err(EvolverError::AlreadyInProgress);
        }

        let id = format!(
            "{}-{}",
            chrono::Utc::now().format("%Y%m%d%H%M%S"),
            &goal.description[..20.min(goal.description.len())]
        );
        self.evolution_id = Some(id.clone());

        // 创建沙箱
        self.status = EvolutionStatus::Analyzing;
        let sandbox = Sandbox::create(&self.repo_path, &id).await?;
        self.current_sandbox = Some(sandbox);

        self.status = EvolutionStatus::Running;
        Ok(())
    }

    /// 获取当前 diff
    pub async fn current_diff(&self) -> Result<String> {
        match &self.current_sandbox {
            Some(s) => s.diff().await,
            None => Err(EvolverError::NotFound("无活跃沙箱".into())),
        }
    }

    /// 确认合并
    pub async fn approve(&mut self) -> Result<()> {
        match &self.current_sandbox {
            Some(s) => {
                self.status = EvolutionStatus::Merging;
                s.merge().await?;
                self.status = EvolutionStatus::Done;
                self.current_sandbox = None;
                Ok(())
            }
            None => Err(EvolverError::NotFound("无活跃沙箱".into())),
        }
    }

    /// 拒绝并丢弃
    pub async fn reject(&mut self) -> Result<()> {
        match &self.current_sandbox {
            Some(s) => {
                s.discard().await?;
                self.status = EvolutionStatus::Discarded;
                self.current_sandbox = None;
                Ok(())
            }
            None => Err(EvolverError::NotFound("无活跃沙箱".into())),
        }
    }
}
