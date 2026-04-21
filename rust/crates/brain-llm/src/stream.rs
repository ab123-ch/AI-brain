//! SSE (Server-Sent Events) stream parser for OpenAI-compatible APIs.
//!
//! Two modes:
//!   1. `parse_sse_body()` — batch parse entire response text
//!   2. `stream_openai_incremental()` — true incremental via `bytes_stream`

use crate::error::{LlmError, Result};
use crate::types::{FinishReason, StreamEvent, TokenUsage};

// ---------------------------------------------------------------------------
// SSE Line Parser (batch mode)
// ---------------------------------------------------------------------------

/// Parse a complete SSE response body into stream events.
///
/// The input is the raw text body from the streaming response,
/// consisting of lines like:
///
/// ```text
/// data: {"id":"...","choices":[{"delta":{"content":"Hello"}}]}
/// data: {"id":"...","choices":[{"delta":{"content":" world"}}]}
/// data: [DONE]
/// ```
#[allow(clippy::too_many_lines)]
pub fn parse_sse_body(body: &str) -> Result<Vec<StreamEvent>> {
    let mut events = Vec::new();
    let mut tool_call_states: Vec<ToolCallAccumulator> = Vec::new();

    for line in body.lines() {
        let line = line.trim();

        if line.is_empty() || line.starts_with(':') {
            continue;
        }

        let data = line.strip_prefix("data: ").unwrap_or(line);

        if data == "[DONE]" {
            finalize_tool_calls(&mut tool_call_states, &mut events);
            events.push(StreamEvent::Done {
                finish_reason: None,
                usage: None,
            });
            break;
        }

        let chunk: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        process_chunk(&chunk, &mut tool_call_states, &mut events);
    }

    Ok(events)
}

// ---------------------------------------------------------------------------
// Incremental SSE frame extraction
// ---------------------------------------------------------------------------

/// Extract the next complete SSE frame from a byte buffer.
///
/// SSE frames are delimited by `\n\n`. Returns the frame content
/// (between `data: ` prefix and `\n\n` delimiter) if a complete frame
/// is available, otherwise `None`.
pub fn extract_sse_frame(buffer: &mut Vec<u8>) -> Option<String> {
    // Find \n\n delimiter
    let delimiter = b"\n\n";
    let pos = buffer
        .windows(delimiter.len())
        .position(|w| w == delimiter)?;

    let frame_bytes: Vec<u8> = buffer.drain(..pos + delimiter.len()).collect();

    // Parse the frame: find "data: " lines
    let frame_str = String::from_utf8_lossy(&frame_bytes);
    let mut data_lines = Vec::new();

    for line in frame_str.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(data) = line.strip_prefix("data: ") {
            data_lines.push(data.to_string());
        } else if !line.starts_with("data:") {
            // Not a data line, skip
        }
    }

    if data_lines.is_empty() {
        return None;
    }

    // Join multiple data lines (SSE spec allows this)
    Some(data_lines.join("\n"))
}

/// Parse a single SSE data payload into a StreamEvent (if valid).
pub fn parse_single_sse_data(data: &str) -> Option<Vec<StreamEvent>> {
    if data.trim() == "[DONE]" {
        return Some(vec![StreamEvent::Done {
            finish_reason: None,
            usage: None,
        }]);
    }

    let chunk: serde_json::Value = serde_json::from_str(data).ok()?;
    let mut events = Vec::new();
    let mut tool_call_states: Vec<ToolCallAccumulator> = Vec::new();
    process_chunk(&chunk, &mut tool_call_states, &mut events);

    if events.is_empty() {
        None
    } else {
        Some(events)
    }
}

// ---------------------------------------------------------------------------
// Shared chunk processing
// ---------------------------------------------------------------------------

/// Process a single JSON chunk, emitting events for any text/tool deltas.
fn process_chunk(
    chunk: &serde_json::Value,
    tool_call_states: &mut Vec<ToolCallAccumulator>,
    events: &mut Vec<StreamEvent>,
) {
    let Some(choices) = chunk.get("choices").and_then(|c| c.as_array()) else {
        return;
    };

    for choice in choices {
        let finish = choice
            .get("finish_reason")
            .and_then(|f| f.as_str())
            .map(FinishReason::from_api_str);

        if let Some(delta) = choice.get("delta") {
            // Text content
            if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                if !content.is_empty() {
                    events.push(StreamEvent::TextDelta {
                        text: content.into(),
                    });
                }
            }

            // Reasoning/thinking content (some OpenAI-compatible APIs)
            if let Some(reasoning) = delta
                .get("reasoning_content")
                .and_then(|c| c.as_str())
            {
                if !reasoning.is_empty() {
                    events.push(StreamEvent::ThinkingDelta {
                        content: reasoning.into(),
                    });
                }
            }

            // Tool calls
            if let Some(tool_calls) = delta
                .get("tool_calls")
                .and_then(serde_json::Value::as_array)
            {
                for tc in tool_calls {
                    let idx = tc
                        .get("index")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0) as usize;

                    while tool_call_states.len() <= idx {
                        tool_call_states.push(ToolCallAccumulator::default());
                    }

                    let acc = &mut tool_call_states[idx];

                    if let Some(id) = tc.get("id").and_then(serde_json::Value::as_str) {
                        acc.id = id.into();
                    }
                    if let Some(func) = tc.get("function") {
                        if let Some(name) = func.get("name").and_then(serde_json::Value::as_str) {
                            acc.name = name.into();
                        }
                        if let Some(args) =
                            func.get("arguments").and_then(serde_json::Value::as_str)
                        {
                            acc.arguments.push_str(args);
                        }
                    }
                }
            }
        }

        // Handle finish
        if let Some(reason) = finish {
            if reason == FinishReason::ToolUse {
                finalize_tool_calls(tool_call_states, events);
            }

            let usage = chunk.get("usage").map(|u| TokenUsage {
                prompt_tokens: u
                    .get("prompt_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                completion_tokens: u
                    .get("completion_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
                total_tokens: u
                    .get("total_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            });

            events.push(StreamEvent::Done {
                finish_reason: Some(reason),
                usage,
            });
        }
    }
}

/// Finalize accumulated tool calls into events.
fn finalize_tool_calls(
    tool_call_states: &mut Vec<ToolCallAccumulator>,
    events: &mut Vec<StreamEvent>,
) {
    for tc in tool_call_states.drain(..) {
        events.push(StreamEvent::ToolCallStart {
            id: tc.id.clone(),
            name: tc.name.clone(),
        });
        events.push(StreamEvent::ToolCallDelta {
            tool_use_id: tc.id,
            delta: tc.arguments,
        });
    }
}

/// Accumulator for streaming tool call arguments.
#[derive(Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
}

// ---------------------------------------------------------------------------
// Batch streaming (collect all events)
// ---------------------------------------------------------------------------

/// Perform a streaming completion request and collect all events.
pub async fn stream_openai(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    body: &serde_json::Value,
) -> Result<Vec<StreamEvent>> {
    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(body)
        .send()
        .await
        .map_err(|e| LlmError::RequestFailed(format!("Stream request failed: {e}")))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(LlmError::ApiError {
            status: status.as_u16(),
            message: body,
        });
    }

    let body = response
        .text()
        .await
        .map_err(|e| LlmError::StreamError(format!("Failed to read stream body: {e}")))?;

    parse_sse_body(&body)
}

// ---------------------------------------------------------------------------
// Incremental streaming (true real-time via bytes_stream)
// ---------------------------------------------------------------------------

/// Perform an incremental streaming completion request.
///
/// Uses `response.bytes_stream()` to yield `StreamEvent` items in real-time
/// as SSE frames arrive from the server. Returns a `mpsc::Receiver` that
/// consumers can `.recv()` from asynchronously.
pub async fn stream_openai_incremental(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    body: &serde_json::Value,
) -> Result<tokio::sync::mpsc::Receiver<StreamEvent>> {
    use futures::StreamExt;

    let response = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(body)
        .send()
        .await
        .map_err(|e| LlmError::RequestFailed(format!("Stream request failed: {e}")))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(LlmError::ApiError {
            status: status.as_u16(),
            message: body,
        });
    }

    let (tx, rx) = tokio::sync::mpsc::channel(256);

    // Spawn a background task to drive the byte stream
    tokio::spawn(async move {
        let mut buffer: Vec<u8> = Vec::with_capacity(4096);
        let mut stream = response.bytes_stream();

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(bytes) => {
                    buffer.extend_from_slice(&bytes);

                    // Extract and process all complete frames
                    while let Some(frame_data) = extract_sse_frame(&mut buffer) {
                        if let Some(events) = parse_single_sse_data(&frame_data) {
                            for event in events {
                                if tx.send(event).await.is_err() {
                                    // Receiver dropped, stop
                                    return;
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Stream chunk error: {e}");
                    break;
                }
            }
        }

        // Process any remaining data in buffer
        if !buffer.is_empty() {
            let remaining = String::from_utf8_lossy(&buffer);
            for line in remaining.lines() {
                let line = line.trim();
                if let Some(data) = line.strip_prefix("data: ") {
                    if let Some(events) = parse_single_sse_data(data) {
                        for event in events {
                            if tx.send(event).await.is_err() {
                                return;
                            }
                        }
                    }
                }
            }
        }
    });

    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_text_stream() {
        let body = r#"data: {"id":"chatcmpl-1","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}

data: {"id":"chatcmpl-1","choices":[{"index":0,"delta":{"content":" world"},"finish_reason":null}]}

data: {"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]
"#;
        let events = parse_sse_body(body).unwrap();
        assert!(events.len() >= 3);
        assert!(matches!(&events[0], StreamEvent::TextDelta { text } if text == "Hello"));
        assert!(matches!(&events[1], StreamEvent::TextDelta { text } if text == " world"));
        // Last event should be Done
        assert!(matches!(events.last(), Some(StreamEvent::Done { .. })));
    }

    #[test]
    fn parse_tool_call_stream() {
        let body = r#"data: {"id":"chatcmpl-2","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_001","function":{"name":"read_file","arguments":""}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-2","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-2","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"/tmp/test.rs\"}"}}]},"finish_reason":null}]}

data: {"id":"chatcmpl-2","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: [DONE]
"#;
        let events = parse_sse_body(body).unwrap();
        // Should have ToolCallStart + ToolCallDelta + Done
        let has_tool_start = events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolCallStart { .. }));
        let has_tool_delta = events
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolCallDelta { .. }));
        let has_done = events.iter().any(|e| matches!(e, StreamEvent::Done { .. }));
        assert!(has_tool_start);
        assert!(has_tool_delta);
        assert!(has_done);
    }

    #[test]
    fn parse_empty_stream() {
        let body = "data: [DONE]\n";
        let events = parse_sse_body(body).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            StreamEvent::Done {
                finish_reason: None,
                usage: None
            }
        ));
    }

    #[test]
    fn skip_malformed_chunks() {
        let body = r#"data: {"id":"chatcmpl-3","choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":null}]}

data: not-json

data: {"id":"chatcmpl-3","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

data: [DONE]
"#;
        let events = parse_sse_body(body).unwrap();
        assert!(events.len() >= 2);
    }

    #[test]
    fn parse_with_usage() {
        let body = r#"data: {"id":"chatcmpl-4","choices":[{"index":0,"delta":{"content":"test"},"finish_reason":null}]}

data: {"id":"chatcmpl-4","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}

data: [DONE]
"#;
        let events = parse_sse_body(body).unwrap();
        let done_event = events
            .iter()
            .find(|e| matches!(e, StreamEvent::Done { .. }));
        assert!(done_event.is_some());
        if let Some(StreamEvent::Done { usage, .. }) = done_event {
            assert_eq!(usage.as_ref().unwrap().total_tokens, 15);
        }
    }
}
