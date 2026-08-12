use std::fs;
use std::path::{Path, PathBuf};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Layer;

/// 默认配置文件模板（带注释）
const CONFIG_TEMPLATE: &str = r#"# AI Brain 配置文件
# 首次运行时自动生成，修改后重启生效

[llm]
default_provider = "xiaomi"
default_model = "mimo-7b"

# ── 厂商配置 ──────────────────────────────────────
[llm.providers.xiaomi]
api_base = "https://xiaomi-llm.example.com/v1"
# 优先从环境变量读取 API Key（安全）
api_key_env = "XIAOMI_API_KEY"
# 也可以直接配置（不推荐提交到版本库）
# api_key = "your-api-key-here"

[llm.providers.deepseek]
api_base = "https://api.deepseek.com/v1"
api_key_env = "DEEPSEEK_API_KEY"

[llm.providers.zhipu]
api_base = "https://open.bigmodel.cn/api/paas/v4"
api_key_env = "ZHIPU_API_KEY"

[llm.providers.gemini]
# Gemini 通过 OpenAI 兼容中转调用
api_base = "https://ai.xfws88.com/v1"
api_key_env = "GEMINI_API_KEY"
kind = "openai"

# ── 每脑独立厂商（未配置的脑走 default_provider）────
[llm.brain_providers]
main = "xiaomi"
memory = "xiaomi"
eval = "deepseek"
evolver = "xiaomi"

[llm.brain_models]
main = "mimo-7b"
sensory = "mimo-7b"
reasoning = "mimo-7b"
memory = "mimo-7b"
eval = "deepseek-chat"
evolver = "mimo-7b"

# ── Web 实例可选模型目录（修改后重启生效）────────────
# 每个协作实例保存条目的 id，并按该条目指定的 provider/model 调用。
[[llm.instance_models]]
id = "deepseek-v4-pro"
label = "DeepSeek V4 Pro"
provider = "deepseek"
model = "deepseek-v4-pro"

[[llm.instance_models]]
id = "deepseek-v4-flash"
label = "DeepSeek V4 Flash"
provider = "deepseek"
model = "deepseek-v4-flash"

[[llm.instance_models]]
id = "gemini-2-5-pro"
label = "Gemini 2.5 Pro"
provider = "gemini"
model = "gemini-2.5-pro"

[[llm.instance_models]]
id = "gemini-2-5-flash"
label = "Gemini 2.5 Flash"
provider = "gemini"
model = "gemini-2.5-flash"

[llm.defaults]
max_tokens = 4096
temperature = 0.7

# 每个脑的独立生成参数（未配置的脑走 defaults）
# max_tokens = 单次输出的 token 上限
[llm.brain_params.main]
max_tokens = 32768
temperature = 0.7

[llm.brain_params.memory]
max_tokens = 32768
temperature = 0.3

[llm.brain_params.eval]
max_tokens = 16384
temperature = 0.3

[llm.brain_params.evolver]
max_tokens = 16384
temperature = 0.3

[llm.brain_params.sensory]
max_tokens = 8192
temperature = 0.3

[llm.brain_params.reasoning]
max_tokens = 8192
temperature = 0.5

[llm.brain_params.compact]
max_tokens = 4096
temperature = 0.3

# ── Tailscale 私有远程访问 ─────────────────────────
# 首次登录 Tailscale 并批准系统网络权限后，普通 web 启动会自动恢复私有 HTTPS 入口。
[remote_access]
enabled = true
port = 8080
# 首次成功连接后由 AI Brain 自动写入固定的 https://<设备>.<Tailnet>.ts.net 地址。
# url = ""
"#;

/// 初始化 AI Brain 运行环境
///
/// 返回 (base_dir, is_first_run)
pub fn init_environment() -> (PathBuf, bool) {
    let base_dir = base_dir();
    let config_path = base_dir.join("config.toml");

    let is_first_run = !config_path.exists();

    // 确保目录结构
    let dirs = [
        base_dir.join("logs"),
        base_dir.join("memory"),
        base_dir.join("sessions"),
        base_dir.join("weights"),
    ];
    for dir in &dirs {
        if let Err(e) = fs::create_dir_all(dir) {
            tracing::warn!("创建目录 {:?} 失败: {e}", dir);
        }
    }

    // 首次运行：生成配置模板
    if is_first_run {
        if let Err(e) = fs::write(&config_path, CONFIG_TEMPLATE) {
            tracing::warn!("写入配置模板失败: {e}");
        }
    }

    (base_dir, is_first_run)
}

/// 打印首次运行引导信息
pub fn print_first_run_guide() {
    eprintln!();
    eprintln!("========================================");
    eprintln!("  AI Brain 首次运行");
    eprintln!("========================================");
    eprintln!();
    eprintln!("  已生成默认配置文件:");
    eprintln!("  ~/.ai-brain/config.toml");
    eprintln!();
    eprintln!("  要使用 AI Brain，需要配置 LLM API Key:");
    eprintln!();
    eprintln!("    方式1（推荐）：设置环境变量");
    eprintln!("      export XIAOMI_API_KEY=your-key");
    eprintln!();
    eprintln!("    方式2：编辑配置文件");
    eprintln!("      vi ~/.ai-brain/config.toml");
    eprintln!("      在 [llm.providers.xiaomi] 下添加 api_key = \"your-key\"");
    eprintln!();
    eprintln!("  配置完成后重新运行 ai-brain 即可。");
    eprintln!("========================================");
    eprintln!();
}

/// 构造一次可安装的日志分发器，便于启动与局部测试共享同一配置。
pub fn build_logging_dispatch(
    base_dir: &Path,
    is_tui: bool,
) -> Result<(tracing::Dispatch, PathBuf), String> {
    let log_dir = base_dir.join("logs");
    fs::create_dir_all(&log_dir)
        .map_err(|error| format!("创建日志目录失败 {}: {error}", log_dir.display()))?;
    let date = chrono::Local::now().format("%Y-%m-%d");
    let log_path = log_dir.join(format!("brain-{date}.log"));
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| format!("打开日志文件失败 {}: {error}", log_path.display()))?;

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::sync::Mutex::new(file))
        .with_ansi(false)
        .with_filter(
            tracing_subscriber::filter::Targets::new()
                .with_default(tracing_subscriber::filter::LevelFilter::DEBUG)
                .with_target("brain_llm", tracing_subscriber::filter::LevelFilter::INFO),
        );
    let dispatch = if is_tui {
        tracing::Dispatch::new(tracing_subscriber::registry().with(file_layer))
    } else {
        let terminal_filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        let terminal_layer = tracing_subscriber::fmt::layer().with_filter(terminal_filter);
        tracing::Dispatch::new(
            tracing_subscriber::registry()
                .with(file_layer)
                .with(terminal_layer),
        )
    };
    Ok((dispatch, log_path))
}

/// 全局安装日志分发器。每个进程只能调用一次。
pub fn init_logging(base_dir: &Path, is_tui: bool) -> Result<PathBuf, String> {
    let (dispatch, log_path) = build_logging_dispatch(base_dir, is_tui)?;
    tracing::dispatcher::set_global_default(dispatch)
        .map_err(|error| format!("注册全局日志分发器失败: {error}"))?;
    Ok(log_path)
}

/// AI Brain 根目录
pub fn base_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".ai-brain")
}

#[cfg(test)]
mod tests {
    use super::{build_logging_dispatch, CONFIG_TEMPLATE};

    #[test]
    fn config_template_documents_instance_model_catalog_and_compatible_gemini_proxy() {
        assert!(CONFIG_TEMPLATE.contains("[[llm.instance_models]]"));
        assert!(CONFIG_TEMPLATE.contains("id = \"gemini-2-5-flash\""));
        assert!(CONFIG_TEMPLATE.contains("api_base = \"https://ai.xfws88.com/v1\""));
        assert!(CONFIG_TEMPLATE.contains("kind = \"openai\""));
        assert!(CONFIG_TEMPLATE.contains("[remote_access]"));
        assert!(CONFIG_TEMPLATE.contains("enabled = true"));
        assert!(CONFIG_TEMPLATE.contains("port = 8080"));
    }

    fn assert_logging_dispatch_writes_file(is_tui: bool) {
        let directory = tempfile::tempdir().unwrap();
        let marker = format!("logging-test-{}", uuid::Uuid::new_v4());
        let (dispatch, path) = build_logging_dispatch(directory.path(), is_tui).unwrap();

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::info!("{marker}");
        });
        drop(dispatch);

        let content = std::fs::read_to_string(path).unwrap();
        assert!(!content.is_empty());
        assert!(content.contains(&marker));
    }

    #[test]
    fn logging_non_tui_dispatch_writes_file() {
        assert_logging_dispatch_writes_file(false);
    }

    #[test]
    fn logging_tui_dispatch_writes_file() {
        assert_logging_dispatch_writes_file(true);
    }

    #[test]
    fn logging_file_suppresses_brain_llm_debug_payloads() {
        let directory = tempfile::tempdir().unwrap();
        let secret = "PROVIDER_RAW_DEBUG_SECRET";
        let (dispatch, path) = build_logging_dispatch(directory.path(), true).unwrap();

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::debug!(target: "brain_llm::openai_compat", "{secret}");
            tracing::info!(target: "brain_llm::openai_compat", "Provider status only");
        });
        drop(dispatch);

        let content = std::fs::read_to_string(path).unwrap();
        assert!(
            !content.contains(secret),
            "文件日志泄漏 Provider DEBUG 正文"
        );
        assert!(content.contains("Provider status only"));
    }

    #[test]
    fn logging_initialization_failure_is_reported() {
        let directory = tempfile::tempdir().unwrap();
        let blocked_base = directory.path().join("blocked-base");
        std::fs::write(&blocked_base, "not a directory").unwrap();

        let Err(error) = build_logging_dispatch(&blocked_base, false) else {
            panic!("日志目录不可创建时必须返回错误");
        };

        assert!(error.contains("创建日志目录失败"), "{error}");
    }
}
