use crate::error::{EvolverError, Result};
use crate::guard::Guard;
use std::path::{Path, PathBuf};

/// Git Worktree 沙箱命令执行结果
pub struct CommandResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Git Worktree 沙箱
pub struct Sandbox {
    pub repo_path: PathBuf,
    pub worktree_path: PathBuf,
    pub branch_name: String,
    pub evolution_id: String,
    guard: Guard,
}

impl Sandbox {
    /// 创建沙箱：git worktree add + 新分支
    pub async fn create(repo_path: &Path, evolution_id: &str) -> Result<Self> {
        let claw_dir = repo_path.join(".claw");
        tokio::fs::create_dir_all(&claw_dir).await?;
        let worktree_path = claw_dir.join(format!("evo-{evolution_id}"));
        let branch_name = format!("evo/{evolution_id}");

        if worktree_path.exists() {
            return Err(EvolverError::Sandbox(format!(
                "沙箱已存在: {:?}",
                worktree_path
            )));
        }

        // git worktree add
        let output = tokio::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                &branch_name,
                worktree_path.to_str().unwrap_or("."),
                "HEAD",
            ])
            .current_dir(repo_path)
            .output()
            .await
            .map_err(|e| EvolverError::Git(format!("创建 worktree 失败: {e}")))?;

        if !output.status.success() {
            return Err(EvolverError::Git(format!(
                "git worktree add 失败: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        let guard = Guard::new(&worktree_path);
        Ok(Self {
            repo_path: repo_path.to_path_buf(),
            worktree_path,
            branch_name,
            evolution_id: evolution_id.to_string(),
            guard,
        })
    }

    /// 在沙箱中执行命令
    pub async fn exec(&self, cmd: &str) -> Result<CommandResult> {
        self.guard.validate_command(cmd)?;

        let parts: Vec<&str> = cmd.split_whitespace().collect();
        let (program, args) = parts
            .split_first()
            .ok_or_else(|| EvolverError::Sandbox("空命令".into()))?;

        let output = tokio::process::Command::new(program)
            .args(args)
            .current_dir(&self.worktree_path)
            .output()
            .await
            .map_err(|e| EvolverError::Sandbox(format!("命令执行失败: {e}")))?;

        Ok(CommandResult {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into(),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }

    /// 读取沙箱中的文件
    pub async fn read_file(&self, path: &Path) -> Result<String> {
        let full_path = self.worktree_path.join(path);
        // 只检查路径是否看起来在沙箱内（文件可能还不存在，不能 canonicalize）
        let guard = Guard::new(&self.worktree_path);
        if full_path.exists() {
            guard.validate_path(&full_path)?;
        }
        tokio::fs::read_to_string(&full_path).await.map_err(Into::into)
    }

    /// 写入文件到沙箱
    pub async fn write_file(&self, path: &Path, content: &str) -> Result<()> {
        let full_path = self.worktree_path.join(path);
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&full_path, content).await.map_err(Into::into)
    }

    /// 获取 diff
    pub async fn diff(&self) -> Result<String> {
        let result = self.exec("git diff HEAD").await?;
        Ok(result.stdout)
    }

    /// 丢弃沙箱
    pub async fn discard(&self) -> Result<()> {
        // git worktree remove
        let output = tokio::process::Command::new("git")
            .args([
                "worktree",
                "remove",
                "--force",
                self.worktree_path.to_str().unwrap_or("."),
            ])
            .current_dir(&self.repo_path)
            .output()
            .await
            .map_err(|e| EvolverError::Git(format!("删除 worktree 失败: {e}")))?;

        if !output.status.success() {
            tracing::warn!(
                "worktree remove 失败: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // git branch -D
        let _ = tokio::process::Command::new("git")
            .args(["branch", "-D", &self.branch_name])
            .current_dir(&self.repo_path)
            .output()
            .await;

        Ok(())
    }

    /// 合并到正式分支（用户确认后调用）
    pub async fn merge(&self) -> Result<()> {
        // 在 worktree 中 commit 所有变更
        self.exec("git add -A").await?;
        self.exec(&format!("git commit -m \"evo: {}\"", self.evolution_id))
            .await?;

        // 回到正式仓库 merge
        let output = tokio::process::Command::new("git")
            .args([
                "merge",
                "--no-ff",
                &self.branch_name,
                "-m",
                &format!("merge: evolution {}", self.evolution_id),
            ])
            .current_dir(&self.repo_path)
            .output()
            .await
            .map_err(|e| EvolverError::Git(format!("merge 失败: {e}")))?;

        if !output.status.success() {
            // merge 失败，abort
            let _ = tokio::process::Command::new("git")
                .args(["merge", "--abort"])
                .current_dir(&self.repo_path)
                .output()
                .await;
            return Err(EvolverError::Git(format!(
                "merge 失败: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        // 打 tag
        let tag = format!("evo-{}", self.evolution_id);
        let _ = tokio::process::Command::new("git")
            .args(["tag", &tag])
            .current_dir(&self.repo_path)
            .output()
            .await;

        // 清理 worktree
        self.discard().await?;

        Ok(())
    }
}
