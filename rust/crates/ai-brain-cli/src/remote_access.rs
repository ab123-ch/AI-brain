//! Tailscale-backed private remote access for the Web UI.

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use thiserror::Error;
use toml_edit::{value, DocumentMut, Item, Table};

const TAILSCALE_BIN_ENV: &str = "AI_BRAIN_TAILSCALE_BIN";
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const STATUS_TIMEOUT_HINT: &str =
    "Tailscale 在 5 秒内没有响应；请打开 Tailscale 应用，完成网络扩展授权和账号登录";
pub const DEFAULT_REMOTE_PORT: u16 = 8080;

/// Machine-local settings stored in `~/.ai-brain/config.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAccessSettings {
    enabled: bool,
    port: u16,
    cached_url: Option<String>,
}

impl Default for RemoteAccessSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            port: DEFAULT_REMOTE_PORT,
            cached_url: None,
        }
    }
}

impl RemoteAccessSettings {
    pub fn load(config_path: &Path) -> Result<Self, RemoteAccessError> {
        if !config_path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(config_path).map_err(|error| {
            RemoteAccessError::Config(format!("读取 {} 失败: {error}", config_path.display()))
        })?;
        let document = content.parse::<toml::Value>().map_err(|error| {
            RemoteAccessError::Config(format!("解析 {} 失败: {error}", config_path.display()))
        })?;
        Self::from_document(&document)
    }

    fn from_document(document: &toml::Value) -> Result<Self, RemoteAccessError> {
        let Some(section) = document.get("remote_access") else {
            return Ok(Self::default());
        };
        let section = section
            .as_table()
            .ok_or_else(|| RemoteAccessError::Config("remote_access 必须是 TOML 表".to_string()))?;

        let enabled = section.get("enabled").map_or(Ok(true), |entry| {
            entry.as_bool().ok_or_else(|| {
                RemoteAccessError::Config("remote_access.enabled 必须是布尔值".to_string())
            })
        })?;
        let port = section
            .get("port")
            .map_or(Ok(i64::from(DEFAULT_REMOTE_PORT)), |entry| {
                entry.as_integer().ok_or_else(|| {
                    RemoteAccessError::Config("remote_access.port 必须是整数".to_string())
                })
            })?;
        let port = u16::try_from(port)
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| {
                RemoteAccessError::Config("remote_access.port 必须在 1..=65535 之间".to_string())
            })?;
        let cached_url = section
            .get("url")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(str::to_string);

        Ok(Self {
            enabled,
            port,
            cached_url,
        })
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn cached_endpoint(&self) -> Option<RemoteEndpoint> {
        self.cached_url
            .as_deref()
            .and_then(RemoteEndpoint::from_https_url)
    }
}

/// The private HTTPS endpoint created by Tailscale Serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEndpoint {
    url: String,
    host: String,
}

impl RemoteEndpoint {
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    fn from_https_url(url: &str) -> Option<Self> {
        let host = url.strip_prefix("https://")?.trim_end_matches('/');
        if host.is_empty()
            || host.contains(['/', ':', '?', '#'])
            || !host.to_ascii_lowercase().ends_with(".ts.net")
        {
            return None;
        }
        let host = host.to_ascii_lowercase();
        Some(Self {
            url: format!("https://{host}"),
            host,
        })
    }
}

#[derive(Debug, Error)]
pub enum RemoteAccessError {
    #[error("未检测到 Tailscale。请先安装并登录 Tailscale：\n  https://tailscale.com/download")]
    NotInstalled,
    #[error("无法读取 Tailscale 状态：{0}")]
    StatusCommand(String),
    #[error(
        "Tailscale 尚未连接（当前状态：{state}）。请打开 Tailscale 完成登录后重试{login_hint}"
    )]
    NotConnected { state: String, login_hint: String },
    #[error("Tailscale 未返回本机的私有 DNS 名称，请确认 MagicDNS 和 HTTPS 已启用")]
    MissingDnsName,
    #[error("远程端口不能为 0")]
    InvalidPort,
    #[error("启用 Tailscale Serve 失败（{0}）。请根据上方提示完成一次性授权后重试")]
    ServeCommand(String),
    #[error("远程访问配置无效：{0}")]
    Config(String),
}

#[derive(Debug, PartialEq, Eq)]
struct TailscaleStatus {
    backend_state: String,
    dns_name: Option<String>,
    auth_url: Option<String>,
}

/// Validate Tailscale connectivity, enable a persistent private Serve proxy,
/// and return the HTTPS endpoint that trusted Tailnet devices can open.
pub fn configure(port: u16) -> Result<RemoteEndpoint, RemoteAccessError> {
    if port == 0 {
        return Err(RemoteAccessError::InvalidPort);
    }

    let executable = find_tailscale_cli().ok_or(RemoteAccessError::NotInstalled)?;
    start_tailscale_service();
    let status_output =
        run_command_output_with_timeout(&executable, &["status", "--json"], STATUS_TIMEOUT)
            .map_err(|error| {
                if error.kind() == io::ErrorKind::TimedOut {
                    RemoteAccessError::StatusCommand(STATUS_TIMEOUT_HINT.to_string())
                } else {
                    RemoteAccessError::StatusCommand(error.to_string())
                }
            })?;
    let status = parse_status_output(&status_output)?;

    if status.backend_state != "Running" {
        let login_hint = status
            .auth_url
            .as_deref()
            .filter(|url| !url.trim().is_empty())
            .map_or_else(String::new, |url| format!("：\n  {url}"));
        return Err(RemoteAccessError::NotConnected {
            state: status.backend_state,
            login_hint,
        });
    }

    let host = status
        .dns_name
        .as_deref()
        .map(str::trim)
        .map(|name| name.trim_end_matches('.'))
        .filter(|name| !name.is_empty())
        .ok_or(RemoteAccessError::MissingDnsName)?
        .to_ascii_lowercase();

    // Inherit stdio because first-time Serve setup can print an HTTPS-consent
    // URL that the user must open. `--bg` persists the proxy across restarts.
    let command_status = Command::new(&executable)
        .args(["serve", "--bg", &port.to_string()])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| RemoteAccessError::ServeCommand(error.to_string()))?;

    if !command_status.success() {
        let detail = command_status
            .code()
            .map_or_else(|| "进程被中断".to_string(), |code| format!("退出码 {code}"));
        return Err(RemoteAccessError::ServeCommand(detail));
    }

    Ok(RemoteEndpoint {
        url: format!("https://{host}"),
        host,
    })
}

/// Return the port only when the Web listener is explicitly bound to a
/// numeric loopback address. Remote access must never widen the listener.
pub fn loopback_port(addr: &str) -> Option<u16> {
    let socket = addr.parse::<SocketAddr>().ok()?;
    (socket.ip() == std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)).then_some(socket.port())
}

/// Save the stable MagicDNS URL while preserving comments and unrelated TOML sections.
pub fn persist_endpoint(
    config_path: &Path,
    endpoint: &RemoteEndpoint,
    port: u16,
) -> Result<(), RemoteAccessError> {
    let content = if config_path.exists() {
        fs::read_to_string(config_path).map_err(|error| {
            RemoteAccessError::Config(format!("读取 {} 失败: {error}", config_path.display()))
        })?
    } else {
        String::new()
    };
    let mut document = content.parse::<DocumentMut>().map_err(|error| {
        RemoteAccessError::Config(format!("解析 {} 失败: {error}", config_path.display()))
    })?;
    if !document.contains_key("remote_access") || !document["remote_access"].is_table() {
        document["remote_access"] = Item::Table(Table::new());
    }
    let section = document["remote_access"]
        .as_table_mut()
        .expect("remote_access was initialized as a table");
    if !section.contains_key("enabled") {
        section["enabled"] = value(true);
    }
    section["port"] = value(i64::from(port));
    section["url"] = value(endpoint.url());

    let parent = config_path.parent().ok_or_else(|| {
        RemoteAccessError::Config(format!("配置路径没有父目录: {}", config_path.display()))
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        RemoteAccessError::Config(format!("创建 {} 失败: {error}", parent.display()))
    })?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| RemoteAccessError::Config(format!("创建临时配置文件失败: {error}")))?;
    if let Ok(metadata) = fs::metadata(config_path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())
            .map_err(|error| RemoteAccessError::Config(format!("保留配置文件权限失败: {error}")))?;
    }
    temporary
        .write_all(document.to_string().as_bytes())
        .and_then(|()| temporary.as_file_mut().sync_all())
        .map_err(|error| RemoteAccessError::Config(format!("写入配置文件失败: {error}")))?;
    temporary.persist(config_path).map_err(|error| {
        RemoteAccessError::Config(format!("替换 {} 失败: {error}", config_path.display()))
    })?;
    Ok(())
}

fn start_tailscale_service() {
    if env::var_os(TAILSCALE_BIN_ENV).is_some_and(|value| !value.is_empty()) {
        return;
    }

    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open")
            .args(["-gj", "-a", "Tailscale"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    #[cfg(target_os = "windows")]
    {
        // The installed Windows service normally starts at boot. This also
        // recovers it after a manual stop when the current user is permitted.
        let _ = Command::new("sc.exe")
            .args(["start", "Tailscale"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn find_tailscale_cli() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(configured) = env::var_os(TAILSCALE_BIN_ENV).filter(|value| !value.is_empty()) {
        candidates.push(PathBuf::from(configured));
    }
    if cfg!(target_os = "macos") {
        candidates.extend([
            PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/Tailscale"),
            PathBuf::from("/Applications/Tailscale.app/Contents/MacOS/tailscale"),
        ]);
        if let Some(home) = dirs::home_dir() {
            candidates.push(home.join("Applications/Tailscale.app/Contents/MacOS/Tailscale"));
            candidates.push(home.join("Applications/Tailscale.app/Contents/MacOS/tailscale"));
        }
    }
    if cfg!(target_os = "windows") {
        candidates.extend(windows_cli_candidates());
        candidates.push(PathBuf::from("tailscale.exe"));
    }
    candidates.push(PathBuf::from("tailscale"));

    candidates.into_iter().find_map(resolve_executable)
}

fn windows_cli_candidates() -> Vec<PathBuf> {
    let program_w6432 = env::var_os("ProgramW6432");
    let program_files = env::var_os("ProgramFiles");
    let program_files_x86 = env::var_os("ProgramFiles(x86)");
    windows_cli_candidates_from(
        program_w6432.as_deref(),
        program_files.as_deref(),
        program_files_x86.as_deref(),
    )
}

fn windows_cli_candidates_from(
    program_w6432: Option<&OsStr>,
    program_files: Option<&OsStr>,
    program_files_x86: Option<&OsStr>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for root in [program_w6432, program_files, program_files_x86]
        .into_iter()
        .flatten()
    {
        let candidate = PathBuf::from(root).join("Tailscale").join("tailscale.exe");
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }

    let default = PathBuf::from(r"C:\Program Files\Tailscale\tailscale.exe");
    if !candidates.contains(&default) {
        candidates.push(default);
    }
    candidates
}

fn resolve_executable(candidate: PathBuf) -> Option<PathBuf> {
    if candidate.is_absolute() || candidate.components().count() > 1 {
        return is_executable_file(&candidate).then_some(candidate);
    }

    let path = env::var_os("PATH")?;
    find_on_path(&candidate, &path)
}

fn find_on_path(name: &std::path::Path, path: &OsStr) -> Option<PathBuf> {
    env::split_paths(path)
        .map(|directory| directory.join(name))
        .find(|candidate| is_executable_file(candidate))
}

fn is_executable_file(path: &std::path::Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn run_command_output_with_timeout(
    executable: &std::path::Path,
    args: &[&str],
    timeout: Duration,
) -> io::Result<Output> {
    let mut child = Command::new(executable)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + timeout;

    loop {
        if child.try_wait()?.is_some() {
            return child.wait_with_output();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::TimedOut, "command timed out"));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn parse_status_output(output: &Output) -> Result<TailscaleStatus, RemoteAccessError> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Ok(status) = parse_status_json(&stdout) {
        return Ok(status);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = [stdout.trim(), stderr.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    Err(RemoteAccessError::StatusCommand(if detail.is_empty() {
        output.status.code().map_or_else(
            || "没有返回状态信息".to_string(),
            |code| format!("退出码 {code}"),
        )
    } else {
        detail
    }))
}

fn parse_status_json(input: &str) -> Result<TailscaleStatus, String> {
    let value: Value = serde_json::from_str(input).map_err(|error| error.to_string())?;
    let backend_state = value
        .get("BackendState")
        .and_then(Value::as_str)
        .ok_or_else(|| "缺少 BackendState".to_string())?
        .to_string();
    let dns_name = value
        .pointer("/Self/DNSName")
        .and_then(Value::as_str)
        .map(str::to_string);
    let auth_url = value
        .get("AuthURL")
        .and_then(Value::as_str)
        .map(str::to_string);

    Ok(TailscaleStatus {
        backend_state,
        dns_name,
        auth_url,
    })
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::time::Duration;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::{
        find_on_path, loopback_port, parse_status_json, persist_endpoint,
        run_command_output_with_timeout, windows_cli_candidates_from, RemoteAccessSettings,
        RemoteEndpoint, TailscaleStatus,
    };

    #[test]
    fn old_config_defaults_to_enabled_remote_access() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "[llm]\ndefault_model = \"test\"\n").unwrap();

        let settings = RemoteAccessSettings::load(&path).unwrap();
        assert!(settings.enabled());
        assert_eq!(settings.port(), 8080);
        assert_eq!(settings.cached_endpoint(), None);
    }

    #[test]
    fn reads_disabled_custom_remote_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            r#"[remote_access]
enabled = false
port = 9090
url = "https://home-brain.example.ts.net"
"#,
        )
        .unwrap();

        let settings = RemoteAccessSettings::load(&path).unwrap();
        assert!(!settings.enabled());
        assert_eq!(settings.port(), 9090);
        assert_eq!(
            settings.cached_endpoint().unwrap().url(),
            "https://home-brain.example.ts.net"
        );
    }

    #[test]
    fn rejects_invalid_remote_port() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, "[remote_access]\nport = 0\n").unwrap();

        let error = RemoteAccessSettings::load(&path).unwrap_err();
        assert!(error.to_string().contains("1..=65535"));
    }

    #[test]
    fn persists_endpoint_without_dropping_comments_or_sections() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            "# keep this comment\n[llm]\ndefault_model = \"test\"\n",
        )
        .unwrap();
        let endpoint = RemoteEndpoint::from_https_url("https://home-brain.example.ts.net").unwrap();

        persist_endpoint(&path, &endpoint, 9090).unwrap();

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("# keep this comment"));
        assert!(content.contains("[llm]"));
        assert!(content.contains("[remote_access]"));
        assert!(content.contains("enabled = true"));
        assert!(content.contains("port = 9090"));
        assert!(content.contains("url = \"https://home-brain.example.ts.net\""));
    }

    #[test]
    fn automatic_remote_access_accepts_only_numeric_loopback_listeners() {
        assert_eq!(loopback_port("127.0.0.1:8080"), Some(8080));
        assert_eq!(loopback_port("[::1]:9090"), None);
        assert_eq!(loopback_port("127.0.0.2:9090"), None);
        assert_eq!(loopback_port("0.0.0.0:8080"), None);
        assert_eq!(loopback_port("192.168.1.10:8080"), None);
        assert_eq!(loopback_port("localhost:8080"), None);
    }

    #[test]
    fn parses_running_status_and_normalizes_fields() {
        let status = parse_status_json(
            r#"{
                "BackendState": "Running",
                "AuthURL": "",
                "Self": { "DNSName": "brain-mac.example.ts.net." }
            }"#,
        )
        .expect("valid status");

        assert_eq!(
            status,
            TailscaleStatus {
                backend_state: "Running".into(),
                dns_name: Some("brain-mac.example.ts.net.".into()),
                auth_url: Some(String::new()),
            }
        );
    }

    #[test]
    fn parses_login_state_without_self_node() {
        let status = parse_status_json(
            r#"{
                "BackendState": "NeedsLogin",
                "AuthURL": "https://login.tailscale.com/a/example"
            }"#,
        )
        .expect("valid login status");

        assert_eq!(status.backend_state, "NeedsLogin");
        assert_eq!(status.dns_name, None);
        assert_eq!(
            status.auth_url.as_deref(),
            Some("https://login.tailscale.com/a/example")
        );
    }

    #[test]
    fn rejects_status_without_backend_state() {
        let error = parse_status_json(r#"{"Self": {}}"#).expect_err("missing state");
        assert!(error.contains("BackendState"));
    }

    #[test]
    fn finds_cli_in_supplied_search_path() {
        let first = tempfile::tempdir().expect("first temp dir");
        let second = tempfile::tempdir().expect("second temp dir");
        let executable = second.path().join("tailscale");
        fs::write(&executable, "test").expect("fake executable");
        #[cfg(unix)]
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .expect("make fake CLI executable");
        let search_path = env::join_paths([first.path(), second.path()]).expect("search path");

        assert_eq!(
            find_on_path(std::path::Path::new("tailscale"), &search_path),
            Some(executable)
        );
        assert_eq!(
            find_on_path(std::path::Path::new("missing-tailscale"), &search_path),
            None
        );
    }

    #[test]
    fn builds_and_deduplicates_windows_cli_candidates() {
        let candidates = windows_cli_candidates_from(
            Some(std::ffi::OsStr::new(r"D:\Program Files")),
            Some(std::ffi::OsStr::new(r"D:\Program Files")),
            Some(std::ffi::OsStr::new(r"D:\Program Files (x86)")),
        );

        assert_eq!(
            candidates,
            vec![
                PathBuf::from(r"D:\Program Files")
                    .join("Tailscale")
                    .join("tailscale.exe"),
                PathBuf::from(r"D:\Program Files (x86)")
                    .join("Tailscale")
                    .join("tailscale.exe"),
                PathBuf::from(r"C:\Program Files\Tailscale\tailscale.exe"),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn terminates_status_process_after_timeout() {
        let error = run_command_output_with_timeout(
            std::path::Path::new("/bin/sleep"),
            &["5"],
            Duration::from_millis(25),
        )
        .expect_err("command should time out");

        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    }

    #[cfg(unix)]
    #[test]
    fn captures_output_from_fast_status_process() {
        let output = run_command_output_with_timeout(
            std::path::Path::new("/bin/sh"),
            &["-c", "printf running"],
            Duration::from_secs(1),
        )
        .expect("command should complete");

        assert!(output.status.success());
        assert_eq!(output.stdout, b"running");
    }
}
