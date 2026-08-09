use std::time::Duration;

/// Provider 共享重试配置。
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// 最大重试次数（不含首次请求）。
    pub max_retries: u32,
    /// 初始退避时间。
    pub initial_backoff: Duration,
    /// 最大退避时间。
    pub max_backoff: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 5,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(16),
        }
    }
}

impl RetryConfig {
    /// 计算第 N 次重试的退避时间（指数退避，带上限）。
    #[must_use]
    pub fn backoff_for_attempt(&self, attempt: u32) -> Duration {
        let multiplier = 1u32
            .checked_shl(attempt.saturating_sub(1))
            .unwrap_or(u32::MAX);
        self.initial_backoff
            .checked_mul(multiplier)
            .map_or(self.max_backoff, |delay| delay.min(self.max_backoff))
    }
}

/// 判断 HTTP 响应是否属于可重试的暂态失败。
#[must_use]
pub fn is_retryable_http_status(status: u16, message: &str) -> bool {
    !is_deterministic_model_route_error(message)
        && matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

/// 判断 reqwest 传输错误是否属于可重试的暂态失败。
#[must_use]
pub fn is_retryable_reqwest_error(error: &reqwest::Error) -> bool {
    if error.is_builder() || error.is_redirect() || error.is_status() {
        return false;
    }

    if error.is_timeout() || error.is_connect() || error.is_request() || error.is_body() {
        return true;
    }

    let mut source = std::error::Error::source(error);
    while let Some(current) = source {
        if let Some(io_error) = current.downcast_ref::<std::io::Error>() {
            if matches!(
                io_error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::UnexpectedEof
            ) {
                return true;
            }
        }
        source = current.source();
    }

    if error.is_decode() {
        return false;
    }

    false
}

fn is_deterministic_model_route_error(message: &str) -> bool {
    let normalized = message.to_lowercase();
    normalized.contains("model_not_found")
        || normalized.contains("model not found")
        || normalized.contains("no available channel")
        || normalized.contains("没有可用渠道")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dropped_connection_is_retryable_without_reading_display_text() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            drop(socket);
        });

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let error = client
            .post(format!("http://{address}/v1/chat/completions"))
            .body("same request")
            .send()
            .await
            .unwrap_err();

        assert!(is_retryable_reqwest_error(&error));
    }

    #[tokio::test]
    async fn truncated_response_body_is_retryable_from_typed_error() {
        use futures::StreamExt;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let response = client
            .get(format!("http://{address}/stream"))
            .send()
            .await
            .unwrap();
        let error = response.bytes_stream().next().await.unwrap().unwrap_err();

        assert!(
            is_retryable_reqwest_error(&error),
            "flags: body={}, decode={}, request={}, source={:?}",
            error.is_body(),
            error.is_decode(),
            error.is_request(),
            std::error::Error::source(&error)
        );
    }

    #[test]
    fn retries_only_transient_http_statuses() {
        for status in [408, 429, 500, 502, 503, 504] {
            assert!(is_retryable_http_status(status, "temporary"));
        }
        for status in [400, 401, 403, 404, 409, 422] {
            assert!(!is_retryable_http_status(status, "temporary"));
        }
    }

    #[test]
    fn deterministic_model_route_503_is_not_retryable() {
        for message in [
            "model_not_found",
            "Model not found in the default group",
            "No available channel",
            "模型没有可用渠道",
        ] {
            assert!(!is_retryable_http_status(503, message));
        }
    }
}
