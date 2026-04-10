use brain_core::types::BrainContext;

/// 收集环境上下文
///
/// 注入日期、工作目录、git 分支、平台等信息。
pub fn gather_context() -> BrainContext {
    BrainContext {
        current_date: chrono::Utc::now().format("%Y-%m-%d").to_string(),
        cwd: std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        git_branch: get_git_branch(),
        platform: get_platform(),
    }
}

fn get_git_branch() -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn get_platform() -> String {
    if cfg!(target_os = "macos") {
        "darwin".into()
    } else if cfg!(target_os = "linux") {
        "linux".into()
    } else if cfg!(target_os = "windows") {
        "windows".into()
    } else {
        "unknown".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gather_context() {
        let ctx = gather_context();
        assert!(!ctx.current_date.is_empty());
        assert!(!ctx.platform.is_empty());
        // git_branch 在测试环境可能是 None，不强制断言
    }
}
