use std::env;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::Command as TokioCommand;
use tokio::runtime::Builder;
use tokio::time::timeout;

use crate::sandbox::{
    build_linux_sandbox_command, resolve_sandbox_status_for_request, FilesystemIsolationMode,
    SandboxConfig, SandboxStatus,
};
use crate::ConfigLoader;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BashCommandInput {
    pub command: String,
    pub timeout: Option<u64>,
    pub description: Option<String>,
    #[serde(rename = "run_in_background")]
    pub run_in_background: Option<bool>,
    #[serde(rename = "dangerouslyDisableSandbox")]
    pub dangerously_disable_sandbox: Option<bool>,
    #[serde(rename = "namespaceRestrictions")]
    pub namespace_restrictions: Option<bool>,
    #[serde(rename = "isolateNetwork")]
    pub isolate_network: Option<bool>,
    #[serde(rename = "filesystemMode")]
    pub filesystem_mode: Option<FilesystemIsolationMode>,
    #[serde(rename = "allowedMounts")]
    pub allowed_mounts: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BashCommandOutput {
    pub stdout: String,
    pub stderr: String,
    #[serde(rename = "rawOutputPath")]
    pub raw_output_path: Option<String>,
    pub interrupted: bool,
    #[serde(rename = "isImage")]
    pub is_image: Option<bool>,
    #[serde(rename = "backgroundTaskId")]
    pub background_task_id: Option<String>,
    #[serde(rename = "backgroundedByUser")]
    pub backgrounded_by_user: Option<bool>,
    #[serde(rename = "assistantAutoBackgrounded")]
    pub assistant_auto_backgrounded: Option<bool>,
    #[serde(rename = "dangerouslyDisableSandbox")]
    pub dangerously_disable_sandbox: Option<bool>,
    #[serde(rename = "returnCodeInterpretation")]
    pub return_code_interpretation: Option<String>,
    #[serde(rename = "noOutputExpected")]
    pub no_output_expected: Option<bool>,
    #[serde(rename = "structuredContent")]
    pub structured_content: Option<Vec<serde_json::Value>>,
    #[serde(rename = "persistedOutputPath")]
    pub persisted_output_path: Option<String>,
    #[serde(rename = "persistedOutputSize")]
    pub persisted_output_size: Option<u64>,
    #[serde(rename = "sandboxStatus")]
    pub sandbox_status: Option<SandboxStatus>,
}

pub fn execute_bash(input: BashCommandInput) -> io::Result<BashCommandOutput> {
    let cwd = env::current_dir()?;
    execute_bash_in_dir(input, &cwd)
}

/// 在显式工作目录中执行 shell 命令。
///
/// `cwd` 必须可规范化为已存在目录；缺失目录不会被创建。规范化后的同一路径同时用于
/// sandbox 解析和前后台进程，且 `.sandbox-*` 辅助目录创建失败会直接返回错误。
pub fn execute_bash_in_dir(input: BashCommandInput, cwd: &Path) -> io::Result<BashCommandOutput> {
    let cwd = cwd.canonicalize()?;
    if !cwd.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("工作路径不是目录: {}", cwd.display()),
        ));
    }
    let sandbox_status = sandbox_status_for_input(&input, &cwd);
    prepare_sandbox_dirs(&cwd)?;

    if input.run_in_background.unwrap_or(false) {
        let mut child = prepare_command(&input.command, &cwd, &sandbox_status);
        let child = child
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        return Ok(BashCommandOutput {
            stdout: String::new(),
            stderr: String::new(),
            raw_output_path: None,
            interrupted: false,
            is_image: None,
            background_task_id: Some(child.id().to_string()),
            backgrounded_by_user: Some(false),
            assistant_auto_backgrounded: Some(false),
            dangerously_disable_sandbox: input.dangerously_disable_sandbox,
            return_code_interpretation: None,
            no_output_expected: Some(true),
            structured_content: None,
            persisted_output_path: None,
            persisted_output_size: None,
            sandbox_status: Some(sandbox_status),
        });
    }

    let runtime = Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(execute_bash_async(input, sandbox_status, cwd))
}

async fn execute_bash_async(
    input: BashCommandInput,
    sandbox_status: SandboxStatus,
    cwd: std::path::PathBuf,
) -> io::Result<BashCommandOutput> {
    let mut command = prepare_tokio_command(&input.command, &cwd, &sandbox_status);

    // 默认超时 120 秒，防止命令无限挂起
    const DEFAULT_TIMEOUT_MS: u64 = 120_000;
    let timeout_ms = input.timeout.unwrap_or(DEFAULT_TIMEOUT_MS);
    let output_result = {
        match timeout(Duration::from_millis(timeout_ms), command.output()).await {
            Ok(result) => (result?, false),
            Err(_) => {
                return Ok(BashCommandOutput {
                    stdout: String::new(),
                    stderr: format!("Command exceeded timeout of {timeout_ms} ms"),
                    raw_output_path: None,
                    interrupted: true,
                    is_image: None,
                    background_task_id: None,
                    backgrounded_by_user: None,
                    assistant_auto_backgrounded: None,
                    dangerously_disable_sandbox: input.dangerously_disable_sandbox,
                    return_code_interpretation: Some(String::from("timeout")),
                    no_output_expected: Some(true),
                    structured_content: None,
                    persisted_output_path: None,
                    persisted_output_size: None,
                    sandbox_status: Some(sandbox_status),
                });
            }
        }
    };

    let (output, interrupted) = output_result;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let no_output_expected = Some(stdout.trim().is_empty() && stderr.trim().is_empty());
    let return_code_interpretation = output.status.code().and_then(|code| {
        if code == 0 {
            None
        } else {
            Some(format!("exit_code:{code}"))
        }
    });

    Ok(BashCommandOutput {
        stdout,
        stderr,
        raw_output_path: None,
        interrupted,
        is_image: None,
        background_task_id: None,
        backgrounded_by_user: None,
        assistant_auto_backgrounded: None,
        dangerously_disable_sandbox: input.dangerously_disable_sandbox,
        return_code_interpretation,
        no_output_expected,
        structured_content: None,
        persisted_output_path: None,
        persisted_output_size: None,
        sandbox_status: Some(sandbox_status),
    })
}

fn sandbox_status_for_input(input: &BashCommandInput, cwd: &std::path::Path) -> SandboxStatus {
    let config = ConfigLoader::default_for(cwd).load().map_or_else(
        |_| SandboxConfig::default(),
        |runtime_config| runtime_config.sandbox().clone(),
    );
    let request = config.resolve_request(
        input.dangerously_disable_sandbox.map(|disabled| !disabled),
        input.namespace_restrictions,
        input.isolate_network,
        input.filesystem_mode,
        input.allowed_mounts.clone(),
    );
    resolve_sandbox_status_for_request(&request, cwd)
}

fn prepare_command(
    command: &str,
    cwd: &std::path::Path,
    sandbox_status: &SandboxStatus,
) -> Command {
    if let Some(launcher) = build_linux_sandbox_command(command, cwd, sandbox_status) {
        let mut prepared = Command::new(launcher.program);
        prepared.args(launcher.args);
        prepared.current_dir(cwd);
        prepared.envs(launcher.env);
        return prepared;
    }

    let mut prepared = Command::new("sh");
    prepared.arg("-lc").arg(command).current_dir(cwd);
    // HOME 重写仅在 namespace 隔离真正生效时启用（Linux + unshare）
    // macOS 上没有真正的沙箱隔离，HOME 重写只会导致工具安装到错误路径
    if sandbox_status.filesystem_active && sandbox_status.namespace_active {
        prepared.env("HOME", cwd.join(".sandbox-home"));
        prepared.env("TMPDIR", cwd.join(".sandbox-tmp"));
    }
    prepared
}

fn prepare_tokio_command(
    command: &str,
    cwd: &std::path::Path,
    sandbox_status: &SandboxStatus,
) -> TokioCommand {
    if let Some(launcher) = build_linux_sandbox_command(command, cwd, sandbox_status) {
        let mut prepared = TokioCommand::new(launcher.program);
        prepared.args(launcher.args);
        prepared.current_dir(cwd);
        prepared.envs(launcher.env);
        return prepared;
    }

    let mut prepared = TokioCommand::new("sh");
    prepared.arg("-lc").arg(command).current_dir(cwd);
    // HOME 重写仅在 namespace 隔离真正生效时启用（Linux + unshare）
    // macOS 上没有真正的沙箱隔离，HOME 重写只会导致工具安装到错误路径
    if sandbox_status.filesystem_active && sandbox_status.namespace_active {
        prepared.env("HOME", cwd.join(".sandbox-home"));
        prepared.env("TMPDIR", cwd.join(".sandbox-tmp"));
    }
    prepared
}

fn prepare_sandbox_dirs(cwd: &std::path::Path) -> io::Result<()> {
    std::fs::create_dir_all(cwd.join(".sandbox-home"))?;
    std::fs::create_dir_all(cwd.join(".sandbox-tmp"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::{execute_bash, execute_bash_in_dir, BashCommandInput};
    use crate::sandbox::FilesystemIsolationMode;

    static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let path = unique_temp_path(name);
            std::fs::create_dir_all(&path).expect("test directory should be created");
            Self { path }
        }

        fn missing(name: &str) -> Self {
            let path = unique_temp_path(name);
            assert!(!path.exists(), "test path should start missing");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn unique_temp_path(name: &str) -> PathBuf {
        let counter = NEXT_TEMP_DIR_ID.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "clawd-native-{name}-{}-{counter}-{timestamp}",
            std::process::id()
        ))
    }

    fn shell_is_available() -> bool {
        match Command::new("sh")
            .arg("-lc")
            .arg("exit 0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(status) => {
                assert!(status.success(), "shell capability check should succeed");
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => panic!("shell capability check failed: {error}"),
        }
    }

    #[test]
    fn executes_simple_command() {
        if !shell_is_available() {
            return;
        }
        let output = execute_bash(BashCommandInput {
            command: String::from("printf 'hello'"),
            timeout: Some(1_000),
            description: None,
            run_in_background: Some(false),
            dangerously_disable_sandbox: Some(false),
            namespace_restrictions: Some(false),
            isolate_network: Some(false),
            filesystem_mode: Some(FilesystemIsolationMode::WorkspaceOnly),
            allowed_mounts: None,
        })
        .expect("bash command should execute");

        assert_eq!(output.stdout, "hello");
        assert!(!output.interrupted);
        assert!(output.sandbox_status.is_some());
    }

    #[test]
    fn disables_sandbox_when_requested() {
        if !shell_is_available() {
            return;
        }
        let output = execute_bash(BashCommandInput {
            command: String::from("printf 'hello'"),
            timeout: Some(1_000),
            description: None,
            run_in_background: Some(false),
            dangerously_disable_sandbox: Some(true),
            namespace_restrictions: None,
            isolate_network: None,
            filesystem_mode: None,
            allowed_mounts: None,
        })
        .expect("bash command should execute");

        assert!(!output.sandbox_status.expect("sandbox status").enabled);
    }

    #[test]
    fn explicit_directory_controls_shell_working_directory() {
        if !shell_is_available() {
            return;
        }
        let original_cwd = std::env::current_dir().expect("current directory should be readable");
        let workspace = TestDir::new("explicit-directory-shell");
        let outside_marker = original_cwd.join("shell-cwd.txt");
        let outside_marker_before = std::fs::read(&outside_marker).ok();

        let output = execute_bash_in_dir(
            BashCommandInput {
                command: String::from("printf 'from-shell' > shell-cwd.txt"),
                timeout: Some(1_000),
                description: None,
                run_in_background: Some(false),
                dangerously_disable_sandbox: Some(true),
                namespace_restrictions: None,
                isolate_network: None,
                filesystem_mode: None,
                allowed_mounts: None,
            },
            workspace.path(),
        )
        .expect("shell command should execute");

        assert!(!output.interrupted, "shell command should not time out");
        assert_eq!(
            output.return_code_interpretation, None,
            "shell command failed: {}",
            output.stderr
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("shell-cwd.txt"))
                .expect("marker should be written in the explicit directory"),
            "from-shell"
        );
        assert_eq!(std::fs::read(&outside_marker).ok(), outside_marker_before);
        assert_eq!(
            std::env::current_dir().expect("current directory should remain readable"),
            original_cwd
        );
    }

    #[test]
    fn explicit_directory_rejects_missing_cwd_without_creating_it() {
        let missing_cwd = TestDir::missing("explicit-directory-missing-cwd");

        let error = execute_bash_in_dir(
            BashCommandInput {
                command: String::from("printf 'should-not-run'"),
                timeout: Some(1_000),
                description: None,
                run_in_background: Some(false),
                dangerously_disable_sandbox: Some(true),
                namespace_restrictions: None,
                isolate_network: None,
                filesystem_mode: None,
                allowed_mounts: None,
            },
            missing_cwd.path(),
        )
        .expect_err("missing cwd should be rejected");

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(
            !missing_cwd.path().exists(),
            "missing cwd must stay missing"
        );
    }

    #[test]
    fn explicit_directory_propagates_sandbox_directory_errors() {
        let workspace = TestDir::new("explicit-directory-sandbox-error");
        std::fs::write(workspace.path().join(".sandbox-home"), "blocking-file")
            .expect("blocking file should be written");

        let error = execute_bash_in_dir(
            BashCommandInput {
                command: String::from("printf 'should-not-run'"),
                timeout: Some(1_000),
                description: None,
                run_in_background: Some(false),
                dangerously_disable_sandbox: Some(true),
                namespace_restrictions: None,
                isolate_network: None,
                filesystem_mode: None,
                allowed_mounts: None,
            },
            workspace.path(),
        )
        .expect_err("sandbox directory creation error should propagate");

        assert_ne!(
            error.kind(),
            std::io::ErrorKind::NotFound,
            "unexpected error source: {error:?}"
        );
    }

    #[test]
    fn explicit_directory_background_uses_cwd_and_prepares_sandbox_dirs() {
        if !shell_is_available() {
            return;
        }
        let workspace = TestDir::new("explicit-directory-background");
        let marker = workspace.path().join("background-cwd.txt");

        let output = execute_bash_in_dir(
            BashCommandInput {
                command: String::from("printf 'from-background' > background-cwd.txt"),
                timeout: Some(1_000),
                description: None,
                run_in_background: Some(true),
                dangerously_disable_sandbox: Some(true),
                namespace_restrictions: None,
                isolate_network: None,
                filesystem_mode: None,
                allowed_mounts: None,
            },
            workspace.path(),
        )
        .expect("background shell command should start");

        assert!(output.background_task_id.is_some());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !marker.is_file() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            std::fs::read_to_string(&marker).expect("background marker should be written"),
            "from-background"
        );
        assert!(workspace.path().join(".sandbox-home").is_dir());
        assert!(workspace.path().join(".sandbox-tmp").is_dir());
    }
}
