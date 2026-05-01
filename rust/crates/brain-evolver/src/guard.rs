use crate::error::{EvolverError, Result};
use std::path::Path;

/// 安全守卫：验证所有进化操作不越界
pub struct Guard {
    sandbox_root: std::path::PathBuf,
}

impl Guard {
    pub fn new(sandbox_root: &Path) -> Self {
        Self {
            sandbox_root: sandbox_root.to_path_buf(),
        }
    }

    /// 验证路径在沙箱内
    pub fn validate_path(&self, path: &Path) -> Result<()> {
        let canonical = path.canonicalize().map_err(|e| {
            EvolverError::GuardViolation(format!("路径无法解析: {e}"))
        })?;
        let root = self.sandbox_root.canonicalize().map_err(|e| {
            EvolverError::GuardViolation(format!("沙箱根无法解析: {e}"))
        })?;
        if canonical.starts_with(&root) {
            Ok(())
        } else {
            Err(EvolverError::GuardViolation(format!(
                "路径 {:?} 不在沙箱 {:?} 内",
                canonical, root
            )))
        }
    }

    /// 验证命令在白名单内
    pub fn validate_command(&self, cmd: &str) -> Result<()> {
        const ALLOWED: &[&str] = &[
            "cargo test",
            "cargo clippy",
            "cargo fmt",
            "cargo check",
            "cargo build",
            "git diff",
            "git log",
            "git status",
            "git commit",
            "git add",
        ];
        let cmd_trimmed = cmd.trim();
        let allowed = ALLOWED.iter().any(|prefix| cmd_trimmed.starts_with(prefix));
        if allowed {
            Ok(())
        } else {
            Err(EvolverError::GuardViolation(format!(
                "命令不在白名单内: {cmd}"
            )))
        }
    }
}
