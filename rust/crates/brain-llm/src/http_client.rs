//! 共享 HTTP 客户端：封装 reqwest::Client 构造（含代理注入）与重试配置。
//!
//! 供 GeminiClient 及未来迁移后的 OpenAiCompatClient 复用，
//! 统一代理、超时、重试策略。

use std::time::Duration;

use crate::error::{LlmError, Result};
use crate::retry::RetryConfig;

/// 共享 HTTP 客户端
pub struct SharedHttpClient {
    client: reqwest::Client,
    retry_config: RetryConfig,
}

impl SharedHttpClient {
    /// 构造客户端。`proxy_url` 为 None 时不走代理。
    ///
    /// 仅支持 `http://` 代理（reqwest 未开 socks feature）。
    pub fn new(proxy_url: Option<&str>, retry_config: RetryConfig) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_mins(5));

        if let Some(url) = proxy_url {
            let parsed = reqwest::Url::parse(url)
                .map_err(|e| LlmError::Config(format!("无效的代理地址 '{url}': {e}")))?;
            if parsed.scheme() != "http" {
                return Err(LlmError::Config(format!(
                    "不支持的代理协议 '{}': 仅支持 http://",
                    parsed.scheme()
                )));
            }
            let proxy = reqwest::Proxy::all(url)
                .map_err(|e| LlmError::Config(format!("无效的代理地址 '{url}': {e}")))?;
            builder = builder.proxy(proxy);
        }

        let client = builder
            .build()
            .map_err(|e| LlmError::RequestFailed(format!("构造 HTTP 客户端失败: {e}")))?;

        Ok(Self {
            client,
            retry_config,
        })
    }

    /// 只读访问内部 reqwest::Client
    #[must_use]
    pub const fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// 只读访问重试配置
    #[must_use]
    pub const fn retry_config(&self) -> &RetryConfig {
        &self.retry_config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_without_proxy_succeeds() {
        let client = SharedHttpClient::new(None, RetryConfig::default());
        assert!(client.is_ok());
    }

    #[test]
    fn build_with_http_proxy_succeeds() {
        let client = SharedHttpClient::new(Some("http://127.0.0.1:7890"), RetryConfig::default());
        assert!(client.is_ok());
    }

    #[test]
    fn build_with_invalid_proxy_fails() {
        let client = SharedHttpClient::new(Some("not-a-url"), RetryConfig::default());
        assert!(client.is_err());
    }

    #[test]
    fn build_with_unsupported_proxy_scheme_fails() {
        let client = SharedHttpClient::new(Some("socks5://127.0.0.1:1080"), RetryConfig::default());
        assert!(client.is_err());
    }

    #[test]
    fn retry_config_preserved() {
        let retry = RetryConfig {
            max_retries: 5,
            ..RetryConfig::default()
        };
        let client = SharedHttpClient::new(None, retry).unwrap();
        assert_eq!(client.retry_config().max_retries, 5);
    }
}
