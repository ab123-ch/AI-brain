# LLM Provider 单次调用自动重试 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 OpenAI 兼容与 Gemini 的普通/流式调用增加额外 5 次结构化自动重试，只补发当前失败请求，并保持现有用户整轮重试不变。

**Architecture:** 在 `brain-llm` 新增共享 `retry` 模块，集中拥有重试配置、HTTP 状态分类与 typed `reqwest::Error` 分类。普通完成和批量流在 Provider/stream 边界循环尝试；增量流在返回 receiver 前完成首事件探测，首事件前失败可重试，首事件后失败通过 `StreamEvent::Error` 报告且不补发。

**Tech Stack:** Rust 2021、Tokio、reqwest 0.12、futures、thiserror、Cargo 内建测试、本地 `TcpListener` 故障注入。

---

## File map

- Create `crates/brain-llm/src/retry.rs`: Provider 无关的重试配置和结构化分类。
- Modify `crates/brain-llm/src/lib.rs`: 注册共享模块并保持 `brain_llm::RetryConfig` 导出。
- Modify `crates/brain-llm/src/error.rs`: 删除 `RequestFailed(String)` 的字符串包含判断。
- Modify `crates/brain-llm/src/http_client.rs`: 从共享模块获取 `RetryConfig`。
- Modify `crates/brain-llm/src/openai_compat.rs`: OpenAI 普通完成和流式重试；保留用户 `.no_proxy()` 改动。
- Modify `crates/brain-llm/src/gemini.rs`: Gemini 普通完成和流式重试。
- Modify `crates/brain-llm/src/stream.rs`: 批量流循环、增量流首事件探测和部分输出错误事件。
- Modify `crates/brain-llm/src/types.rs`: 增加 `StreamEvent::Error`。
- Regression only `crates/brain-main`, `crates/ai-brain-cli`: 不修改用户手动重试代码。

### Task 1: 建立共享 typed 重试策略

**Files:**
- Create: `crates/brain-llm/src/retry.rs`
- Modify: `crates/brain-llm/src/lib.rs`
- Modify: `crates/brain-llm/src/error.rs`
- Modify: `crates/brain-llm/src/http_client.rs`
- Modify: `crates/brain-llm/src/openai_compat.rs`
- Modify: `crates/brain-llm/src/gemini.rs`

- [x] **Step 1: 写默认次数、退避和 HTTP 状态 RED 测试**

```rust
#[test]
fn default_retries_five_times_with_expected_backoff() {
    let retry = RetryConfig::default();
    assert_eq!(retry.max_retries, 5);
    let delays = (1..=5)
        .map(|attempt| retry.backoff_for_attempt(attempt))
        .collect::<Vec<_>>();
    assert_eq!(delays, [1, 2, 4, 8, 16].map(Duration::from_secs));
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
    for body in [
        "model_not_found",
        "Model not found in the default group",
        "No available channel",
        "模型没有可用渠道",
    ] {
        assert!(!is_retryable_http_status(503, body));
    }
}
```

- [x] **Step 2: 运行 RED**

```powershell
cargo test -p brain-llm retry::tests -- --nocapture
```

Expected: FAIL，因为默认仍为额外 2 次，且共享模块不存在。

- [x] **Step 3: 实现最小共享策略**

```rust
#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub initial_backoff: Duration,
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

pub fn is_retryable_http_status(status: u16, body: &str) -> bool {
    !is_deterministic_model_route_error(body)
        && matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

pub fn is_retryable_reqwest_error(error: &reqwest::Error) -> bool {
    if error.is_builder() || error.is_redirect() || error.is_decode() || error.is_status() {
        return false;
    }
    error.is_timeout()
        || error.is_connect()
        || error.is_request()
        || error.is_body()
        || retryable_io_source(error)
}
```

`retryable_io_source` 遍历 `Error::source()`，只接受
`ConnectionReset/ConnectionAborted/BrokenPipe/NotConnected/TimedOut/UnexpectedEof`。
从 OpenAI 模块移走 `RetryConfig`，`lib.rs` 改为 `pub use retry::RetryConfig`。
`LlmError::RequestFailed` 不再解析展示字符串；`ApiError` 委托状态分类。

- [x] **Step 4: 运行 GREEN**

```powershell
cargo fmt -p brain-llm -- --check
cargo test -p brain-llm retry::tests -- --nocapture
cargo test -p brain-llm error::tests -- --nocapture
```

Expected: 全部 PASS。

- [x] **Step 5: 检查 `.no_proxy()`**

```powershell
git diff -- crates/brain-llm/src/openai_compat.rs
```

Expected: 用户原有 `.no_proxy()` 仍存在且未被覆盖。

### Task 2: OpenAI 普通完成只重试当前请求

**Files:**
- Modify: `crates/brain-llm/src/openai_compat.rs`
- Test: `crates/brain-llm/src/openai_compat.rs`

- [x] **Step 1: 写连接中断和逻辑调用隔离 RED 测试**

本地 `TcpListener` 保存每个请求 JSON。测试执行三个逻辑调用：call-1 一次成功；call-2
前五次断线、第六次成功；call-3 一次成功。断言 wire bodies 共 8 个，call-2 六份
完全相同，call-1/call-3 各一次。另测连续六次断线后返回
`RetriesExhausted { attempts: 6, .. }` 且无第七次连接。

测试注入零时长退避：

```rust
RetryConfig {
    max_retries: 5,
    initial_backoff: Duration::ZERO,
    max_backoff: Duration::ZERO,
}
```

- [x] **Step 2: 运行 RED**

```powershell
cargo test -p brain-llm retries_current_request_five_times_without_replaying_previous_call -- --exact --nocapture
cargo test -p brain-llm stops_after_six_total_attempts -- --exact --nocapture
```

Expected: FAIL，当前字符串分类漏判本地断线或耗尽错误没有 attempts=6。

- [x] **Step 3: 实现 typed 分类**

在 `.send().await` 的 `Err(source)` 分支直接调用
`is_retryable_reqwest_error(&source)`；HTTP 分支调用共享状态分类。所有尝试复用循环外生成
的 `api_request`。可重试错误耗尽时返回 `RetriesExhausted`，确定性错误直接返回原错误。

- [x] **Step 4: 运行 GREEN**

```powershell
cargo test -p brain-llm openai_compat::tests -- --nocapture
```

Expected: 新增故障注入与既有解析/脱敏测试全部 PASS。

### Task 3: Gemini 普通完成复用同一合同

**Files:**
- Modify: `crates/brain-llm/src/gemini.rs`
- Test: `crates/brain-llm/src/gemini.rs`

- [x] **Step 1: 写 Gemini RED 测试**

本地服务器返回最小合法 Gemini JSON。证明前五次断线、第六次成功；401 只请求一次；
普通 503 可恢复；含 `No available channel` 的 503 只请求一次。

- [x] **Step 2: 运行 RED**

```powershell
cargo test -p brain-llm gemini_retries_typed_transport_failure_five_times -- --exact --nocapture
```

Expected: FAIL，因为 Gemini 仍通过 `RequestFailed(String)` 分类。

- [x] **Step 3: 实现共享分类调用**

在 Gemini `.send()` typed error 边界调用共享分类，HTTP 分支调用共享状态分类，耗尽语义
与 OpenAI 相同。不更改 URL、认证 header 或响应解析。

- [x] **Step 4: 运行 GREEN**

```powershell
cargo test -p brain-llm gemini::tests -- --nocapture
```

Expected: 全部 PASS，日志脱敏测试继续不出现 secret。

### Task 4: 批量流和首事件前增量流重试

**Files:**
- Modify: `crates/brain-llm/src/types.rs`
- Modify: `crates/brain-llm/src/stream.rs`
- Modify: `crates/brain-llm/src/openai_compat.rs`
- Modify: `crates/brain-llm/src/gemini.rs`
- Test: `crates/brain-llm/src/stream.rs`

- [x] **Step 1: 写流式 RED 合同**

```rust
#[tokio::test]
async fn batch_stream_discards_failed_attempt_events_before_retry() {
    let server = ScriptedSseServer::openai([
        SseAttempt::TextThenDisconnect("discard-me"),
        SseAttempt::Complete("keep-me"),
    ])
    .await;
    let events = stream_openai(
        &reqwest::Client::new(),
        &server.url(),
        "test-key",
        &serde_json::json!({"model":"test","messages":[],"stream":true}),
        &zero_backoff_retry(1),
    )
    .await
    .unwrap();
    let text = events.into_iter().filter_map(|event| match event {
        StreamEvent::TextDelta { text } => Some(text),
        _ => None,
    }).collect::<String>();
    assert_eq!(text, "keep-me");
    assert_eq!(server.request_count(), 2);
}

#[tokio::test]
async fn incremental_stream_retries_before_first_event() {
    let server = ScriptedSseServer::openai([
        SseAttempt::DisconnectBeforeEvent,
        SseAttempt::Complete("recovered"),
    ])
    .await;
    let mut receiver = stream_openai_incremental(
        &reqwest::Client::new(),
        &server.url(),
        "test-key",
        &serde_json::json!({"model":"test","messages":[],"stream":true}),
        &zero_backoff_retry(1),
    )
    .await
    .unwrap();
    assert!(matches!(receiver.recv().await, Some(StreamEvent::TextDelta { text }) if text == "recovered"));
    assert_eq!(server.request_count(), 2);
}

#[tokio::test]
async fn incremental_stream_does_not_retry_after_first_event() {
    let server = ScriptedSseServer::openai([
        SseAttempt::TextThenDisconnect("partial"),
        SseAttempt::Complete("must-not-run"),
    ])
    .await;
    let mut receiver = stream_openai_incremental(
        &reqwest::Client::new(),
        &server.url(),
        "test-key",
        &serde_json::json!({"model":"test","messages":[],"stream":true}),
        &zero_backoff_retry(5),
    )
    .await
    .unwrap();
    assert!(matches!(receiver.recv().await, Some(StreamEvent::TextDelta { text }) if text == "partial"));
    assert!(matches!(receiver.recv().await, Some(StreamEvent::Error { .. })));
    assert_eq!(server.request_count(), 1);
}

#[tokio::test]
async fn dropping_incremental_receiver_stops_driver() {
    let server = ScriptedSseServer::openai([SseAttempt::HoldAfterText("first")]).await;
    let receiver = stream_openai_incremental(
        &reqwest::Client::new(),
        &server.url(),
        "test-key",
        &serde_json::json!({"model":"test","messages":[],"stream":true}),
        &zero_backoff_retry(5),
    )
    .await
    .unwrap();
    drop(receiver);
    server.release_held_connection();
    server.wait_until_idle().await;
    assert_eq!(server.request_count(), 1);
}
```

同一测试模块定义 `ScriptedSseServer`、`SseAttempt`、`zero_backoff_retry`；helper 负责读取
完整请求、按脚本写入合法 SSE 帧，并以原子计数器暴露 request count。OpenAI 与 Gemini
分别使用各自合法 SSE JSON，测试不得访问外网或使用真实 API Key。

- [x] **Step 2: 运行 RED**

```powershell
cargo test -p brain-llm stream::tests -- --nocapture
```

Expected: FAIL；批量流无 retry，增量错误静默关闭且无 Error variant。

- [x] **Step 3: 实现批量流尝试循环**

让 `stream_openai`/`stream_gemini` 接收 `&RetryConfig`。每次尝试覆盖 send、status、完整
body 读取和 SSE parse；send/body typed 暂态错误与白名单状态可重试，完整 body 解析
错误直接返回。失败 attempt 的事件不得暴露。

- [x] **Step 4: 实现增量流首事件探测**

返回 receiver 前建立请求、验证状态并读取到首个可解析事件。首事件前暂态错误进入下一
attempt；得到首批事件后创建 channel 并 spawn 余下 driver。driver 中断时发送：

```rust
StreamEvent::Error { message: safe_message }
```

随后结束，不重新发请求。receiver drop 立即结束 driver。Gemini 的 `call_sequence` 与
`has_tool_use` 必须从探测阶段连续传给后台 driver。

- [x] **Step 5: 运行 GREEN**

```powershell
cargo test -p brain-llm stream::tests -- --nocapture
cargo test -p brain-llm --lib
```

Expected: 全部 PASS，无真实网络请求。

### Task 5: 回归、门禁和部署

**Files:**
- Regression only: `crates/brain-main/src/tool_loop.rs`
- Regression only: `crates/ai-brain-cli/src/web/session_manager.rs`
- Regression only: `crates/ai-brain-cli/src/web/ws_handler.rs`
- Update: `task_plan.md`
- Update: `findings.md`
- Update: `progress.md`

- [x] **Step 1: 验证逻辑调用和用户重试未变化**

```powershell
cargo test -p brain-main tool_loop -- --nocapture
cargo test -p ai-brain-cli --lib retry_last_user_message -- --nocapture
```

Expected: Provider wire retries不增加逻辑 `llm_calls`；Web 用户重试全部 PASS 且生产文件
无 diff。

- [x] **Step 2: 运行仓库要求门禁**

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: 新代码无失败；范围外既有失败需记录退出码和首个阻断点，并补跑受影响包严格
定向门禁。

- [x] **Step 3: 检查范围**

```powershell
git diff --check
git diff --stat
git diff -- crates/brain-llm/src/openai_compat.rs
```

Expected: `retry_last_user_message` 生产文件无 diff；`.no_proxy()` 仍存在；用户小说材料、
`.clawd-todos.json` 未进入本功能暂存区。

- [ ] **Step 4: 重建并重启智脑**

精确核实当前 `ai-brain.exe` PID/命令行/路径后停止该进程，再运行：

```powershell
cargo build --release -p ai-brain-cli --bin ai-brain
```

以隐藏窗口启动 `target/release/ai-brain.exe web --addr 127.0.0.1:8080`，验证新 PID、仅
监听 `127.0.0.1:8080`、HTTP 200 和当天日志非空。不调用真实付费模型做失败 smoke；
故障注入证据来自本地服务器测试。

- [ ] **Step 5: 完成记录**

更新三份规划文件。实施提交前单独审查暂存区；如果无法安全拆分 `openai_compat.rs` 中
用户 `.no_proxy()` hunk，则保持实现未提交并说明，绝不误提交用户改动。
