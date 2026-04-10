use std::fs;
use std::path::{Path, PathBuf};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

/// 默认配置文件模板（带注释）
const CONFIG_TEMPLATE: &str = r#"# AI Brain 配置文件
# 首次运行时自动生成，修改后重启生效

[llm]
default_provider = "zhipu"
default_model = "glm-4.7"

[llm.providers.zhipu]
api_base = "https://open.bigmodel.cn/api/paas/v4"
# 优先从环境变量读取 API Key（安全）
api_key_env = "ZHIPU_API_KEY"
# 也可以直接配置（不推荐提交到版本库）
# api_key = "your-api-key-here"

[llm.brain_models]
# 感知脑 — 轻量解析，用快速模型
sensory = "glm-4.7"
# 推理脑 — 需要强逻辑，用大模型
reasoning = "glm-5.1"
# 记忆脑 — 关键词提取/摘要，轻量即可
memory = "glm-4.7"
# 执行脑 — 工具选择需要推理，用大模型
motor = "glm-5.1"
# 校验脑 — Agent 场景，用快速模型
validation = "glm-5-turbo"

[llm.defaults]
max_tokens = 4096
temperature = 0.7
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
    eprintln!("  当前为回声模式（不会调用真实 AI）");
    eprintln!("  要启用真实 LLM，请配置 API Key:");
    eprintln!();
    eprintln!("    方式1（推荐）：设置环境变量");
    eprintln!("      export ZHIPU_API_KEY=your-key");
    eprintln!();
    eprintln!("    方式2：编辑配置文件");
    eprintln!("      vi ~/.ai-brain/config.toml");
    eprintln!("      取消 api_key 注释并填入密钥");
    eprintln!();
    eprintln!("========================================");
    eprintln!();
}

/// 初始化文件日志
pub fn init_file_logging(base_dir: &Path) {
    let log_dir = base_dir.join("logs");
    let date = chrono::Local::now().format("%Y-%m-%d");
    let log_path = log_dir.join(format!("brain-{date}.log"));

    let file = match fs::OpenOptions::new().create(true).append(true).open(&log_path) {
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

/// AI Brain 根目录
pub fn base_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".ai-brain")
}
