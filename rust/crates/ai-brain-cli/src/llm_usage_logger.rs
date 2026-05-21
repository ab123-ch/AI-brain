//! LLM 使用日志记录器
//!
//! 记录每次 LLM 调用的详细使用情况：
//! - Token 使用量（输入、输出、缓存创建、缓存读取）
//! - 缓存命中率
//! - 成本预估（基于模型定价）
//!
//! 日志路径: ~/.ai-brain/llm_usage.log

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use brain_llm::types::TokenUsage;

/// 模型定价信息（每百万 token 的美元成本）
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    pub input_cost_per_million: f64,
    pub output_cost_per_million: f64,
    pub cache_creation_cost_per_million: f64,
    pub cache_read_cost_per_million: f64,
}

impl ModelPricing {
    /// 根据模型名称获取定价
    pub fn for_model(model: &str) -> Option<Self> {
        let normalized = model.to_ascii_lowercase();

        // Anthropic Claude 模型
        if normalized.contains("claude") {
            if normalized.contains("haiku") {
                return Some(Self {
                    input_cost_per_million: 1.0,
                    output_cost_per_million: 5.0,
                    cache_creation_cost_per_million: 1.25,
                    cache_read_cost_per_million: 0.1,
                });
            }
            if normalized.contains("opus") {
                return Some(Self {
                    input_cost_per_million: 15.0,
                    output_cost_per_million: 75.0,
                    cache_creation_cost_per_million: 18.75,
                    cache_read_cost_per_million: 1.5,
                });
            }
            // Sonnet 或其他 Claude 模型
            return Some(Self {
                input_cost_per_million: 15.0,
                output_cost_per_million: 75.0,
                cache_creation_cost_per_million: 18.75,
                cache_read_cost_per_million: 1.5,
            });
        }

        // DeepSeek 模型
        if normalized.contains("deepseek") {
            return Some(Self {
                input_cost_per_million: 0.27, // DeepSeek-V3 价格
                output_cost_per_million: 1.10,
                cache_creation_cost_per_million: 0.27,
                cache_read_cost_per_million: 0.07,
            });
        }

        // GLM 模型（智谱）
        if normalized.contains("glm") {
            return Some(Self {
                input_cost_per_million: 1.0, // GLM-4 价格估算
                output_cost_per_million: 1.0,
                cache_creation_cost_per_million: 1.0,
                cache_read_cost_per_million: 0.1,
            });
        }

        None
    }
}

/// 单次 LLM 调用的使用记录
#[derive(Debug, Clone)]
pub struct LlmUsageRecord {
    /// 时间戳
    pub timestamp: String,
    /// 模型名称
    pub model: String,
    /// 调用序号（第几次调用）
    pub call_index: u32,
    /// Token 使用情况
    pub usage: TokenUsage,
    /// 缓存命中率（0.0-1.0）
    pub cache_hit_rate: f64,
    /// 预估成本（美元）
    pub estimated_cost_usd: f64,
    /// 是否使用了模型特定定价
    pub has_model_pricing: bool,
}

impl LlmUsageRecord {
    /// 从 TokenUsage 创建记录
    pub fn new(model: String, call_index: u32, usage: TokenUsage) -> Self {
        let cache_hit_rate = usage.cache_hit_rate().unwrap_or(0.0);

        let pricing = ModelPricing::for_model(&model);
        let has_model_pricing = pricing.is_some();

        let estimated_cost_usd = if let Some(pricing) = pricing {
            Self::calculate_cost(&usage, pricing)
        } else {
            // 使用默认定价（Claude Sonnet）
            let default_pricing = ModelPricing {
                input_cost_per_million: 15.0,
                output_cost_per_million: 75.0,
                cache_creation_cost_per_million: 18.75,
                cache_read_cost_per_million: 1.5,
            };
            Self::calculate_cost(&usage, default_pricing)
        };

        Self {
            timestamp: chrono::Local::now()
                .format("%Y-%m-%d %H:%M:%S%.3f")
                .to_string(),
            model,
            call_index,
            usage,
            cache_hit_rate,
            estimated_cost_usd,
            has_model_pricing,
        }
    }

    fn calculate_cost(usage: &TokenUsage, pricing: ModelPricing) -> f64 {
        let input_cost = usage.prompt_tokens as f64 / 1_000_000.0 * pricing.input_cost_per_million;
        let output_cost =
            usage.completion_tokens as f64 / 1_000_000.0 * pricing.output_cost_per_million;
        let cache_creation_cost = usage.cache_creation_input_tokens as f64 / 1_000_000.0
            * pricing.cache_creation_cost_per_million;
        let cache_read_cost = usage.cache_read_input_tokens as f64 / 1_000_000.0
            * pricing.cache_read_cost_per_million;

        input_cost + output_cost + cache_creation_cost + cache_read_cost
    }
}

/// LLM 使用日志记录器
pub struct LlmUsageLogger {
    /// 日志文件路径
    #[allow(dead_code)]
    path: PathBuf,
    /// 文件句柄（Mutex 保证线程安全）
    writer: Mutex<std::fs::File>,
    /// 会话累计使用量
    cumulative_usage: Mutex<TokenUsage>,
    /// 调用计数器
    call_counter: Mutex<u32>,
}

impl LlmUsageLogger {
    /// 创建新的 LLM 使用日志记录器
    ///
    /// 日志文件: `~/.ai-brain/llm_usage.log`
    pub fn new() -> Self {
        let base = crate::init::base_dir();
        let _ = fs::create_dir_all(&base);

        let path = base.join("llm_usage.log");

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_or_else(|e| {
                eprintln!("无法创建 LLM 使用日志 {}: {e}", path.display());
                std::process::exit(1);
            });

        let sl = Self {
            path,
            writer: Mutex::new(file),
            cumulative_usage: Mutex::new(TokenUsage::default()),
            call_counter: Mutex::new(0),
        };

        // 写入会话头
        sl.log_raw(&format!(
            "\n═══════════════════════════════════════════════════════════════════════════════\n\
             LLM 使用日志 | 开始时间: {}\n\
             ═══════════════════════════════════════════════════════════════════════════════\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        ));

        sl
    }

    /// 记录一次 LLM 调用的使用情况
    pub fn log_usage(&self, model: &str, usage: &TokenUsage) {
        // 更新调用计数
        let call_index = {
            let mut counter = self.call_counter.lock().unwrap();
            *counter += 1;
            *counter
        };

        // 更新累计使用量
        {
            let mut cumulative = self.cumulative_usage.lock().unwrap();
            cumulative.prompt_tokens += usage.prompt_tokens;
            cumulative.completion_tokens += usage.completion_tokens;
            cumulative.total_tokens += usage.total_tokens;
            cumulative.cache_creation_input_tokens += usage.cache_creation_input_tokens;
            cumulative.cache_read_input_tokens += usage.cache_read_input_tokens;
        }

        // 创建本次记录
        let record = LlmUsageRecord::new(model.to_string(), call_index, usage.clone());

        // 格式化并写入日志
        let log_entry = self.format_record(&record);
        self.log_raw(&log_entry);
    }

    /// 记录会话结束时的累计统计
    pub fn log_session_summary(&self) {
        let cumulative = self.cumulative_usage.lock().unwrap();
        let call_count = *self.call_counter.lock().unwrap();

        if call_count == 0 {
            return;
        }

        let cache_hit_rate = if cumulative.total_input_tokens() > 0 {
            cumulative.cache_read_input_tokens as f64 / cumulative.total_input_tokens() as f64
        } else {
            0.0
        };

        let summary = format!(
            "\n\
             ─────────────────────────────────────────────────────────────────────────────\n\
             会话累计统计\n\
             ─────────────────────────────────────────────────────────────────────────────\n\
             LLM 调用次数: {}\n\
             总 Token 使用量: {} (输入: {}, 输出: {})\n\
             缓存统计: 创建={}, 读取={}, 命中率={:.1}%\n\
             ─────────────────────────────────────────────────────────────────────────────\n\
             会话结束时间: {}\n\
             ═══════════════════════════════════════════════════════════════════════════════\n",
            call_count,
            cumulative.total_tokens,
            cumulative.prompt_tokens,
            cumulative.completion_tokens,
            cumulative.cache_creation_input_tokens,
            cumulative.cache_read_input_tokens,
            cache_hit_rate * 100.0,
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );

        self.log_raw(&summary);
    }

    fn format_record(&self, record: &LlmUsageRecord) -> String {
        format!(
            "\n\
             [{}] 第{}次调用 | 模型: {}\n\
             ┌─────────────────────────────────────────────────────────\n\
             │ Token 使用量:\n\
             │   输入 (prompt):     {:>8}\n\
             │   输出 (completion): {:>8}\n\
             │   总计:              {:>8}\n\
             │\n\
             │ 缓存统计:\n\
             │   缓存创建:          {:>8}\n\
             │   缓存读取:          {:>8}\n\
             │   缓存命中率:        {:>7.1}%\n\
             │\n\
             │ 成本预估:\n\
             │   预估费用:          ${:.4}\n\
             │   定价来源:          {}\n\
             └─────────────────────────────────────────────────────────\n",
            record.timestamp,
            record.call_index,
            record.model,
            record.usage.prompt_tokens,
            record.usage.completion_tokens,
            record.usage.total_tokens,
            record.usage.cache_creation_input_tokens,
            record.usage.cache_read_input_tokens,
            record.cache_hit_rate * 100.0,
            record.estimated_cost_usd,
            if record.has_model_pricing {
                "模型特定定价"
            } else {
                "默认定价 (Claude Sonnet)"
            }
        )
    }

    fn log_raw(&self, text: &str) {
        if let Ok(mut file) = self.writer.lock() {
            let _ = file.write_all(text.as_bytes());
            let _ = file.flush();
        }
    }
}

/// 全局 LLM 使用日志记录器实例
static LLM_USAGE_LOGGER: std::sync::OnceLock<LlmUsageLogger> = std::sync::OnceLock::new();

/// 获取全局 LLM 使用日志记录器
pub fn get_logger() -> &'static LlmUsageLogger {
    LLM_USAGE_LOGGER.get_or_init(LlmUsageLogger::new)
}

/// 记录 LLM 使用情况（便捷函数）
pub fn log_llm_usage(model: &str, usage: &TokenUsage) {
    get_logger().log_usage(model, usage);
}

/// 记录会话结束统计（便捷函数）
pub fn log_session_summary() {
    get_logger().log_session_summary();
}
