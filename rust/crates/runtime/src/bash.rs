use std::env;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::ffi::OsStringExt;

use serde::{Deserialize, Serialize};
use tokio::process::Command as TokioCommand;
use tokio::runtime::Builder;
use tokio::time::timeout;

use crate::sandbox::{
    build_linux_sandbox_command, resolve_sandbox_status_for_request, FilesystemIsolationMode,
    SandboxConfig, SandboxStatus,
};
use crate::{
    ConfigError, ConfigLoader, HostPlatform, ResolvedCommandBackend, ResolvedCommandExecution,
};

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
    #[serde(rename = "executionBackend")]
    pub execution_backend: Option<String>,
    #[serde(rename = "commandSyntax")]
    pub command_syntax: Option<String>,
    #[serde(rename = "hostWorkingDirectory")]
    pub host_working_directory: Option<String>,
    #[serde(rename = "wslDistribution")]
    pub wsl_distribution: Option<String>,
    #[serde(rename = "wslUser")]
    pub wsl_user: Option<String>,
    #[serde(rename = "backgroundTaskIdKind")]
    pub background_task_id_kind: Option<String>,
}

#[derive(Debug, Clone)]
struct CommandLauncher {
    backend: ResolvedCommandBackend,
    program: OsString,
    args: Vec<OsString>,
    current_dir: PathBuf,
    env: Vec<(OsString, OsString)>,
}

trait ExecutableLocator {
    fn wsl(&self) -> io::Result<OsString>;
    fn powershell(&self) -> io::Result<OsString>;
}

struct SystemExecutableLocator;

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
    validate_canonical_working_directory(&cwd)?;
    let config = ConfigLoader::default_for(&cwd)
        .load()
        .map_err(|error| config_error_to_io(&error))?;
    let execution = config
        .command_execution()
        .resolve(HostPlatform::current())
        .map_err(|error| config_error_to_io(&error))?;
    let sandbox_config = config.sandbox().clone();
    execute_bash_resolved(
        input,
        &cwd,
        &execution,
        &sandbox_config,
        &SystemExecutableLocator,
    )
}

/// Execute a command with an already-resolved, immutable backend.
pub fn execute_bash_with_execution_in_dir(
    input: BashCommandInput,
    cwd: &Path,
    execution: &ResolvedCommandExecution,
) -> io::Result<BashCommandOutput> {
    let cwd = cwd.canonicalize()?;
    validate_canonical_working_directory(&cwd)?;
    validate_command_working_directory(execution, &cwd)?;
    let sandbox_config = ConfigLoader::default_for(&cwd).load().map_or_else(
        |_| SandboxConfig::default(),
        |config| config.sandbox().clone(),
    );
    execute_bash_resolved(
        input,
        &cwd,
        execution,
        &sandbox_config,
        &SystemExecutableLocator,
    )
}

/// Validate host and working-directory invariants without starting a process.
pub fn validate_command_working_directory(
    execution: &ResolvedCommandExecution,
    cwd: &Path,
) -> io::Result<()> {
    execution
        .validate()
        .map_err(|error| config_error_to_io(&error))?;
    if execution.host_platform != HostPlatform::current() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "frozen command backend host {} does not match current host {}",
                execution.host_platform.as_str(),
                HostPlatform::current().as_str()
            ),
        ));
    }
    if execution.backend == ResolvedCommandBackend::Wsl {
        windows_path_for_wsl(cwd)?;
    }
    Ok(())
}

fn validate_canonical_working_directory(cwd: &Path) -> io::Result<()> {
    if cwd.is_dir() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("工作路径不是目录: {}", cwd.display()),
        ))
    }
}

fn config_error_to_io(error: &ConfigError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, error.to_string())
}

fn execute_bash_resolved(
    input: BashCommandInput,
    cwd: &Path,
    execution: &ResolvedCommandExecution,
    sandbox_config: &SandboxConfig,
    locator: &impl ExecutableLocator,
) -> io::Result<BashCommandOutput> {
    let sandbox_status = sandbox_status_for_input(&input, cwd, sandbox_config);
    prepare_sandbox_dirs(cwd)?;
    let launcher =
        build_command_launcher(&input.command, cwd, execution, &sandbox_status, locator)?;
    debug_assert_eq!(launcher.backend, execution.backend);

    if input.run_in_background.unwrap_or(false) {
        let mut child = std_command(&launcher);
        let child = child
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        return Ok(command_output(
            &input,
            execution,
            cwd,
            sandbox_status,
            String::new(),
            String::new(),
            false,
            Some(child.id().to_string()),
            None,
        ));
    }

    let runtime = Builder::new_current_thread().enable_all().build()?;
    runtime.block_on(execute_bash_async(
        input,
        execution.clone(),
        sandbox_status,
        launcher,
    ))
}

async fn execute_bash_async(
    input: BashCommandInput,
    execution: ResolvedCommandExecution,
    sandbox_status: SandboxStatus,
    launcher: CommandLauncher,
) -> io::Result<BashCommandOutput> {
    const DEFAULT_TIMEOUT_MS: u64 = 120_000;

    let cwd = launcher.current_dir.clone();
    let mut command = tokio_command(&launcher);
    command.kill_on_drop(true);

    let timeout_ms = input.timeout.unwrap_or(DEFAULT_TIMEOUT_MS);
    let output = match timeout(Duration::from_millis(timeout_ms), command.output()).await {
        Ok(result) => result?,
        Err(_) => {
            return Ok(command_output(
                &input,
                &execution,
                &cwd,
                sandbox_status,
                String::new(),
                format!("Command exceeded timeout of {timeout_ms} ms"),
                true,
                None,
                Some(String::from("timeout")),
            ));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let return_code_interpretation = output.status.code().and_then(|code| {
        if code == 0 {
            None
        } else {
            Some(format!("exit_code:{code}"))
        }
    });
    Ok(command_output(
        &input,
        &execution,
        &cwd,
        sandbox_status,
        stdout,
        stderr,
        false,
        None,
        return_code_interpretation,
    ))
}

#[allow(clippy::too_many_arguments)]
fn command_output(
    input: &BashCommandInput,
    execution: &ResolvedCommandExecution,
    cwd: &Path,
    sandbox_status: SandboxStatus,
    stdout: String,
    stderr: String,
    interrupted: bool,
    background_task_id: Option<String>,
    return_code_interpretation: Option<String>,
) -> BashCommandOutput {
    let is_background = background_task_id.is_some();
    let no_output = stdout.trim().is_empty() && stderr.trim().is_empty();
    BashCommandOutput {
        stdout,
        stderr,
        raw_output_path: None,
        interrupted,
        is_image: None,
        background_task_id,
        backgrounded_by_user: is_background.then_some(false),
        assistant_auto_backgrounded: is_background.then_some(false),
        dangerously_disable_sandbox: input.dangerously_disable_sandbox,
        return_code_interpretation,
        no_output_expected: Some(no_output),
        structured_content: None,
        persisted_output_path: None,
        persisted_output_size: None,
        sandbox_status: Some(sandbox_status),
        execution_backend: Some(execution.backend.as_str().to_string()),
        command_syntax: Some(execution.syntax.as_str().to_string()),
        host_working_directory: Some(cwd.display().to_string()),
        wsl_distribution: execution.wsl_distribution.clone(),
        wsl_user: execution.wsl_user.clone(),
        background_task_id_kind: (is_background
            && execution.backend == ResolvedCommandBackend::Wsl)
            .then(|| String::from("windows-launcher-pid")),
    }
}

fn sandbox_status_for_input(
    input: &BashCommandInput,
    cwd: &Path,
    config: &SandboxConfig,
) -> SandboxStatus {
    let request = config.resolve_request(
        input.dangerously_disable_sandbox.map(|disabled| !disabled),
        input.namespace_restrictions,
        input.isolate_network,
        input.filesystem_mode,
        input.allowed_mounts.clone(),
    );
    resolve_sandbox_status_for_request(&request, cwd)
}

fn build_command_launcher(
    command: &str,
    cwd: &Path,
    execution: &ResolvedCommandExecution,
    sandbox_status: &SandboxStatus,
    locator: &impl ExecutableLocator,
) -> io::Result<CommandLauncher> {
    execution
        .validate()
        .map_err(|error| config_error_to_io(&error))?;
    match execution.backend {
        ResolvedCommandBackend::Wsl => {
            let args = wsl_args(command, cwd, execution)?;
            Ok(CommandLauncher {
                backend: execution.backend,
                program: locator.wsl()?,
                args,
                current_dir: cwd.to_path_buf(),
                env: Vec::new(),
            })
        }
        ResolvedCommandBackend::Powershell => Ok(CommandLauncher {
            backend: execution.backend,
            program: locator.powershell()?,
            args: vec![
                OsString::from("-NoProfile"),
                OsString::from("-NonInteractive"),
                OsString::from("-Command"),
                OsString::from(command),
            ],
            current_dir: cwd.to_path_buf(),
            env: Vec::new(),
        }),
        ResolvedCommandBackend::Sh => {
            if let Some(launcher) = build_linux_sandbox_command(command, cwd, sandbox_status) {
                return Ok(CommandLauncher {
                    backend: execution.backend,
                    program: OsString::from(launcher.program),
                    args: launcher.args.into_iter().map(OsString::from).collect(),
                    current_dir: cwd.to_path_buf(),
                    env: launcher
                        .env
                        .into_iter()
                        .map(|(key, value)| (OsString::from(key), OsString::from(value)))
                        .collect(),
                });
            }
            let mut env = Vec::new();
            if sandbox_status.filesystem_active && sandbox_status.namespace_active {
                env.push((OsString::from("HOME"), cwd.join(".sandbox-home").into()));
                env.push((OsString::from("TMPDIR"), cwd.join(".sandbox-tmp").into()));
            }
            Ok(CommandLauncher {
                backend: execution.backend,
                program: OsString::from("sh"),
                args: vec![OsString::from("-lc"), OsString::from(command)],
                current_dir: cwd.to_path_buf(),
                env,
            })
        }
    }
}

fn wsl_args(
    command: &str,
    cwd: &Path,
    execution: &ResolvedCommandExecution,
) -> io::Result<Vec<OsString>> {
    let mut args = Vec::new();
    if let Some(distribution) = &execution.wsl_distribution {
        args.push(OsString::from("--distribution"));
        args.push(OsString::from(distribution));
    }
    if let Some(user) = &execution.wsl_user {
        args.push(OsString::from("--user"));
        args.push(OsString::from(user));
    }
    args.push(OsString::from("--cd"));
    args.push(windows_path_for_wsl(cwd)?);
    args.push(OsString::from("--exec"));
    args.push(OsString::from("/bin/bash"));
    args.push(OsString::from("-lc"));
    args.push(OsString::from(command));
    Ok(args)
}

fn windows_path_for_wsl(cwd: &Path) -> io::Result<OsString> {
    let value = cwd.as_os_str().to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "WSL working directory is not valid Unicode: {}",
                cwd.display()
            ),
        )
    })?;
    let normalized = value.strip_prefix(r"\\?\").unwrap_or(value);
    if normalized.starts_with(r"UNC\")
        || normalized.starts_with(r"\\")
        || normalized.starts_with(r"\.\")
        || normalized.starts_with(r"\?\")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "WSL backend requires a drive-letter working directory, not UNC/device path: {value}"
            ),
        ));
    }
    let bytes = normalized.as_bytes();
    if bytes.len() < 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !matches!(bytes[2], b'\\' | b'/')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("WSL backend requires an absolute Windows drive path: {value}"),
        ));
    }
    Ok(OsString::from(normalized))
}

fn std_command(launcher: &CommandLauncher) -> Command {
    let mut command = Command::new(&launcher.program);
    command
        .args(&launcher.args)
        .current_dir(&launcher.current_dir)
        .envs(launcher.env.iter().cloned());
    command
}

fn tokio_command(launcher: &CommandLauncher) -> TokioCommand {
    let mut command = TokioCommand::new(&launcher.program);
    command
        .args(&launcher.args)
        .current_dir(&launcher.current_dir)
        .envs(launcher.env.iter().cloned());
    command
}

impl ExecutableLocator for SystemExecutableLocator {
    fn wsl(&self) -> io::Result<OsString> {
        let executable = windows_system_directory()?.join("wsl.exe");
        if executable.is_file() {
            Ok(executable.into_os_string())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "WSL executable not found at {}; install WSL or configure commandExecution.backend as powershell or sh",
                    executable.display()
                ),
            ))
        }
    }

    fn powershell(&self) -> io::Result<OsString> {
        if command_exists("pwsh.exe") {
            return Ok(OsString::from("pwsh.exe"));
        }
        let powershell = windows_system_directory()?
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        if powershell.is_file() {
            Ok(powershell.into_os_string())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "PowerShell executable not found (expected pwsh.exe or powershell.exe)",
            ))
        }
    }
}

#[cfg(windows)]
fn windows_system_directory() -> io::Result<PathBuf> {
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    let mut buffer = vec![0_u16; 32_768];
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
    if length == 0 || length as usize >= buffer.len() {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(length as usize);
    Ok(PathBuf::from(OsString::from_wide(&buffer)))
}

#[cfg(not(windows))]
fn windows_system_directory() -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows command backend is unavailable on this host",
    ))
}

#[cfg(windows)]
fn command_exists(command: &str) -> bool {
    windows_system_directory().is_ok_and(|system_dir| {
        Command::new(system_dir.join("where.exe"))
            .arg(command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

#[cfg(not(windows))]
fn command_exists(_command: &str) -> bool {
    false
}

fn prepare_sandbox_dirs(cwd: &std::path::Path) -> io::Result<()> {
    std::fs::create_dir_all(cwd.join(".sandbox-home"))?;
    std::fs::create_dir_all(cwd.join(".sandbox-tmp"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::ffi::OsString;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::{
        build_command_launcher, execute_bash, execute_bash_in_dir, sandbox_status_for_input,
        windows_path_for_wsl, BashCommandInput, ExecutableLocator,
    };
    use crate::sandbox::{FilesystemIsolationMode, SandboxConfig};
    use crate::{CommandSyntax, HostPlatform, ResolvedCommandBackend, ResolvedCommandExecution};

    static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDir {
        path: PathBuf,
    }

    struct FakeExecutableLocator {
        wsl_calls: Cell<usize>,
        powershell_calls: Cell<usize>,
        fail_wsl: bool,
    }

    impl FakeExecutableLocator {
        fn available() -> Self {
            Self {
                wsl_calls: Cell::new(0),
                powershell_calls: Cell::new(0),
                fail_wsl: false,
            }
        }

        fn missing_wsl() -> Self {
            Self {
                fail_wsl: true,
                ..Self::available()
            }
        }
    }

    impl ExecutableLocator for FakeExecutableLocator {
        fn wsl(&self) -> io::Result<OsString> {
            self.wsl_calls.set(self.wsl_calls.get() + 1);
            if self.fail_wsl {
                Err(io::Error::new(io::ErrorKind::NotFound, "missing WSL"))
            } else {
                Ok(OsString::from(r"C:\Windows\System32\wsl.exe"))
            }
        }

        fn powershell(&self) -> io::Result<OsString> {
            self.powershell_calls.set(self.powershell_calls.get() + 1);
            Ok(OsString::from("pwsh.exe"))
        }
    }

    fn command_input(command: &str) -> BashCommandInput {
        BashCommandInput {
            command: command.to_string(),
            timeout: Some(1_000),
            description: None,
            run_in_background: Some(false),
            dangerously_disable_sandbox: Some(true),
            namespace_restrictions: None,
            isolate_network: None,
            filesystem_mode: None,
            allowed_mounts: None,
        }
    }

    fn resolved_backend(backend: ResolvedCommandBackend) -> ResolvedCommandExecution {
        ResolvedCommandExecution {
            backend,
            syntax: if backend == ResolvedCommandBackend::Powershell {
                CommandSyntax::Powershell
            } else {
                CommandSyntax::Posix
            },
            host_platform: HostPlatform::Windows,
            wsl_distribution: None,
            wsl_user: None,
        }
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
    fn wsl_launcher_preserves_argument_boundaries_and_unicode_cwd() {
        let locator = FakeExecutableLocator::available();
        let mut execution = resolved_backend(ResolvedCommandBackend::Wsl);
        execution.wsl_distribution = Some("Ubuntu-24.04".into());
        execution.wsl_user = Some("brain".into());
        let cwd = Path::new(r"C:\Work Space\项目");
        let input = command_input("printf '%s' \"a b;$PATH\"");
        let sandbox = sandbox_status_for_input(&input, cwd, &SandboxConfig::default());

        let launcher = build_command_launcher(&input.command, cwd, &execution, &sandbox, &locator)
            .expect("WSL launcher");

        assert_eq!(launcher.program, r"C:\Windows\System32\wsl.exe");
        assert_eq!(
            launcher.args,
            [
                "--distribution",
                "Ubuntu-24.04",
                "--user",
                "brain",
                "--cd",
                r"C:\Work Space\项目",
                "--exec",
                "/bin/bash",
                "-lc",
                "printf '%s' \"a b;$PATH\"",
            ]
            .map(OsString::from)
        );
        assert_eq!(launcher.current_dir, cwd);
        assert_eq!(locator.wsl_calls.get(), 1);
        assert_eq!(locator.powershell_calls.get(), 0);
    }

    #[test]
    fn wsl_path_normalization_strips_drive_namespace_and_rejects_unc() {
        assert_eq!(
            windows_path_for_wsl(Path::new(r"\\?\C:\workspace\repo")).unwrap(),
            r"C:\workspace\repo"
        );
        for invalid in [
            r"\\server\share\repo",
            r"\\wsl.localhost\Ubuntu\home\repo",
            r"\\?\UNC\server\share\repo",
            r"\\.\C:\device",
            r"relative\repo",
        ] {
            let error = windows_path_for_wsl(Path::new(invalid)).expect_err("invalid WSL cwd");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{invalid}");
        }
    }

    #[test]
    fn powershell_launcher_uses_noninteractive_command_contract() {
        let locator = FakeExecutableLocator::available();
        let execution = resolved_backend(ResolvedCommandBackend::Powershell);
        let cwd = Path::new(r"C:\workspace\repo");
        let input = command_input("Get-Location");
        let sandbox = sandbox_status_for_input(&input, cwd, &SandboxConfig::default());

        let launcher = build_command_launcher(&input.command, cwd, &execution, &sandbox, &locator)
            .expect("PowerShell launcher");
        assert_eq!(launcher.program, "pwsh.exe");
        assert_eq!(
            launcher.args,
            ["-NoProfile", "-NonInteractive", "-Command", "Get-Location"].map(OsString::from)
        );
        assert_eq!(locator.wsl_calls.get(), 0);
        assert_eq!(locator.powershell_calls.get(), 1);
    }

    #[test]
    fn missing_wsl_does_not_fall_back_to_powershell() {
        let locator = FakeExecutableLocator::missing_wsl();
        let execution = resolved_backend(ResolvedCommandBackend::Wsl);
        let cwd = Path::new(r"C:\workspace\repo");
        let input = command_input("pwd");
        let sandbox = sandbox_status_for_input(&input, cwd, &SandboxConfig::default());

        let error = build_command_launcher(&input.command, cwd, &execution, &sandbox, &locator)
            .expect_err("missing WSL should fail");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(locator.wsl_calls.get(), 1);
        assert_eq!(locator.powershell_calls.get(), 0);
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
        assert_eq!(output.execution_backend.as_deref(), Some("sh"));
        assert_eq!(output.command_syntax.as_deref(), Some("posix"));
        assert!(output.host_working_directory.is_some());
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
        assert_eq!(output.execution_backend.as_deref(), Some("sh"));
        assert_eq!(output.background_task_id_kind, None);
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
