//! Shared construction for transports backed by the durable collaboration runtime.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use brain_llm::config::LlmConfig;

use crate::orchestrator::Orchestrator;

use super::collaboration::{default_runtime_dir, CollaborationConfig, CollaborationRepository};
use super::collaboration_runtime::CollaborationRuntime;

pub struct CollaborationApplication {
    pub orchestrator: Arc<Orchestrator>,
    pub runtime: Arc<CollaborationRuntime>,
    pub repository: Arc<CollaborationRepository>,
    pub workspace_root: PathBuf,
}

impl CollaborationApplication {
    pub async fn start(orch: Orchestrator, workspace_root: &Path) -> Result<Self, String> {
        let workspace_root = std::fs::canonicalize(workspace_root)
            .map_err(|error| format!("读取服务启动工作目录失败: {error}"))?;
        let runtime_dir = default_runtime_dir();
        let llm_config = Arc::new(match LlmConfig::load_default() {
            Ok(config) => config,
            Err(error) => {
                tracing::warn!("加载实例模型目录失败，仅保留 main 策略: {error}");
                LlmConfig::default_config()
            }
        });
        let model_policy_details = llm_config.available_instance_model_policies();
        let collaboration_config = CollaborationConfig::load(&runtime_dir.join("config.toml"))
            .map_err(|error| format!("加载协作配置失败: {error}"))?
            .with_available_model_policies(
                model_policy_details
                    .iter()
                    .map(|policy| policy.policy_id.clone()),
            );
        let repository = Arc::new(
            CollaborationRepository::new_with_startup_working_directory(
                &runtime_dir,
                collaboration_config,
                &workspace_root,
            )
            .map_err(|error| format!("初始化协作存储失败: {error}"))?,
        );
        let orchestrator = Arc::new(orch);
        let runtime = CollaborationRuntime::start(
            Arc::clone(&repository),
            Arc::clone(&orchestrator),
            llm_config,
            model_policy_details,
        )
        .await
        .map_err(|error| format!("启动协作运行时失败: {error}"))?;

        Ok(Self {
            orchestrator,
            runtime,
            repository,
            workspace_root,
        })
    }
}
