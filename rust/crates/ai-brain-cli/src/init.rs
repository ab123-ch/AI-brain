use std::fs;
use std::path::{Path, PathBuf};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
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

# ── 每脑独立厂商（未配置的脑走 default_provider）────
[llm.brain_providers]
main = "xiaomi"
memory = "xiaomi"
eval = "deepseek"
evolver = "xiaomi"
novel = "xiaomi"

[llm.brain_models]
main = "mimo-7b"
sensory = "mimo-7b"
reasoning = "mimo-7b"
memory = "mimo-7b"
eval = "deepseek-chat"
evolver = "mimo-7b"
novel = "mimo-7b"

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

[llm.brain_params.novel]
max_tokens = 32768
temperature = 0.9

[llm.brain_params.sensory]
max_tokens = 8192
temperature = 0.3

[llm.brain_params.reasoning]
max_tokens = 8192
temperature = 0.5

[llm.brain_params.compact]
max_tokens = 4096
temperature = 0.3
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

/// 初始化文件日志（普通模式：stdout + 文件）
pub fn init_file_logging(base_dir: &Path) {
    let log_dir = base_dir.join("logs");
    let date = chrono::Local::now().format("%Y-%m-%d");
    let log_path = log_dir.join(format!("brain-{date}.log"));

    let file = match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!("无法打开日志文件 {:?}: {e}", log_path);
            return;
        }
    };

    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::sync::Mutex::new(file))
        .with_ansi(false)
        .with_filter(tracing_subscriber::filter::LevelFilter::DEBUG);

    // 在现有 subscriber 上叠加文件日志
    let _ = tracing_subscriber::registry().with(file_layer).try_init();
}

/// 初始化 TUI 模式日志（只写文件，不写终端）
pub fn init_tui_logging(base_dir: &Path) {
    let log_dir = base_dir.join("logs");
    let _ = fs::create_dir_all(&log_dir);
    let date = chrono::Local::now().format("%Y-%m-%d");
    let log_path = log_dir.join(format!("brain-{date}.log"));

    match fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(file) => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
                )
                .with_writer(std::sync::Mutex::new(file))
                .with_ansi(false)
                .init();
        }
        Err(_) => {
            // 连日志文件都打不开，全部丢弃
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::new("warn"))
                .with_writer(std::io::sink)
                .init();
        }
    }
}

/// AI Brain 根目录
pub fn base_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".ai-brain")
}
