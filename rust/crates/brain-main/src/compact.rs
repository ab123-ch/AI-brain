//! LLM 智能压缩引擎
//!
//! 两条核心流程：
//! A. 滑动窗口式后台预压缩 — 每轮 turn 结束后后台静默压缩刚滑出窗口的历史 turn
//! B. 阈值触发替换 — 达到警告阈值时将 pending 的压缩结果替换进去，零阻塞

use std::collections::{HashMap, HashSet};

use brain_core::types::{ConversationMessage, MessageRole};
use brain_llm::{ChatMessage, ChatRequest, LlmProvider};

// ─── 配置 ──────────────────────────────────────────────────────────

/// 压缩配置
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// 保留最近 N 轮不动（每轮 = User + 中间消息），默认 4
    pub preserve_recent_turns: usize,
    /// 单轮工具结果字符数 > 此值触发预压缩，默认 10000
    pub tool_result_compress_threshold: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            preserve_recent_turns: 4,
            tool_result_compress_threshold: 10_000,
        }
    }
}

/// 压缩结果
#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// 被压缩的消息组数
    pub compacted_groups: usize,
    /// 节省的字符数
    pub chars_saved: usize,
}

// ─── 消息分组 ──────────────────────────────────────────────────────

/// 一个 turn block：从 User 消息开始，到下一个 User 消息之前的所有消息
#[derive(Debug)]
#[allow(dead_code)]
struct TurnBlock {
    /// User 消息在总消息列表中的索引
    start: usize,
    /// block 结束索引（exclusive）
    end: usize,
    /// 该 block 中 Tool 消息的索引列表
    tool_indices: Vec<usize>,
    /// 该 block 中 Tool 消息的总字符数
    tool_chars: usize,
}

/// 按 User 消息分割为 turn blocks
fn split_turn_blocks(messages: &[ConversationMessage]) -> Vec<TurnBlock> {
    let mut blocks = Vec::new();
    let mut current_start: Option<usize> = None;
    let mut tool_indices = Vec::new();
    let mut tool_chars = 0usize;

    for (i, msg) in messages.iter().enumerate() {
        if msg.role == MessageRole::User {
            // 保存上一个 block
            if let Some(start) = current_start {
                blocks.push(TurnBlock {
                    start,
                    end: i,
                    tool_indices: std::mem::take(&mut tool_indices),
                    tool_chars,
                });
                tool_chars = 0;
            }
            current_start = Some(i);
        } else if msg.role == MessageRole::Tool {
            tool_indices.push(i);
            tool_chars += msg.content.chars().count();
        }
    }

    // 最后一个 block
    if let Some(start) = current_start {
        blocks.push(TurnBlock {
            start,
            end: messages.len(),
            tool_indices,
            tool_chars,
        });
    }

    blocks
}

// ─── 找到滑出窗口的 turn ──────────────────────────────────────────

/// 找到刚滑出保留窗口的单个 turn block 的起止位置
///
/// 逻辑：从前往后找到第一个**未压缩**且不在保留窗口内的 turn block
/// 返回 None 如果没有可压缩的 turn
pub fn find_slide_out_turn(
    messages: &[ConversationMessage],
    compressed_indices: &HashSet<usize>,
    config: &CompactionConfig,
) -> Option<(usize, usize)> {
    let blocks = split_turn_blocks(messages);
    if blocks.is_empty() {
        return None;
    }

    // 保留最近 N 个 turn blocks
    let preserve_count = config.preserve_recent_turns;
    if blocks.len() <= preserve_count {
        return None;
    }

    // 找到不在保留窗口内的最靠后的 turn block
    let eligible_end = blocks.len() - preserve_count;
    for block in blocks.iter().take(eligible_end) {
        // 检查该 block 中是否有消息已被压缩
        let any_compressed = (block.start..block.end)
            .any(|i| compressed_indices.contains(&i));
        if !any_compressed && !block.tool_indices.is_empty() {
            return Some((block.start, block.end));
        }
    }

    None
}

// ─── 压缩水印 ─────────────────────────────────────────────────────

#[allow(dead_code)]
const COMPRESSION_WATERMARK: &str = "[工具调用结果已压缩]";

#[allow(dead_code)]
fn build_compressed_content(task: &str, chain: &str, key_info: &str, conclusion: &str) -> String {
    format!(
        "{COMPRESSION_WATERMARK} 以下是调用后的结论和关键内容。\n\
         如有疑问，优先根据标注的文件路径和行号重新浏览。\n\n\
         任务：{task}\n\
         调用链路：{chain}\n\
         关键信息：\n{key_info}\n\
         结论：{conclusion}"
    )
}

// ─── LLM 压缩提示词 ──────────────────────────────────────────────

fn build_compaction_prompt(
    user_message: &str,
    tool_results: &str,
    assistant_conclusion: &str,
) -> String {
    format!(
        r#"你是一个上下文压缩引擎。请分析以下对话轮次，提取关键信息，丢弃冗余细节。

## 用户意图
{user_message}

## 工具调用结果
{tool_results}

## 助手结论
{assistant_conclusion}

## 压缩要求
1. 参考助手结论，理解工具调用发现了什么
2. 保留调用链路（哪个方法调了哪个方法）
3. 保留关键文件信息（路径、行数范围、方法/函数名）
4. 保留关键结论（问题根因、修改内容等）
5. 丢弃：完整代码内容、完整 grep 输出、中间过程日志

## 输出格式（直接输出，不要多余解释）
任务：{{一句话描述任务意图}}
调用链路：{{关键调用路径}}
关键信息：
  - {{文件路径}}（行 {{xx}}-{{yy}}，{{方法名}}）：{{关键发现}}
  - ...
结论：{{助手总结的核心结论}}"#
    )
}

// ─── 单轮压缩 ──────────────────────────────────────────────────────

/// 压缩单个 turn block（给 LLM 看完整工具结果）
///
/// `messages` 是完整的消息列表（或快照），`turn_start`/`turn_end` 指定要压缩的范围。
/// 返回 (消息索引 → 压缩后内容) 的映射，只包含 Tool 消息。
pub async fn compress_single_turn(
    messages: &[ConversationMessage],
    turn_start: usize,
    turn_end: usize,
    llm: &dyn LlmProvider,
) -> HashMap<usize, String> {
    let block = &messages[turn_start..turn_end];

    // 1. 提取用户消息
    let user_message: String = block
        .iter()
        .find(|m| m.role == MessageRole::User)
        .map(|m| m.content.clone())
        .unwrap_or_default();

    // 2. 提取工具结果（完整内容）
    let tool_results: String = block
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n---\n");

    // 3. 提取助手结论
    let assistant_conclusion: String = block
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Assistant)
        .map(|m| m.content.clone())
        .unwrap_or_default();

    if tool_results.is_empty() {
        return HashMap::new();
    }

    // 4. 构建 LLM 请求
    let prompt = build_compaction_prompt(&user_message, &tool_results, &assistant_conclusion);
    let request = ChatRequest {
        model: None,
        messages: vec![ChatMessage::user(prompt)],
        max_tokens: Some(2048),
        temperature: Some(0.3),
        tools: None,
        tool_choice: None,
    };

    // 5. 调用 LLM
    match llm.complete(request).await {
        Ok(response) => {
            let compressed_text = response.text();
            if compressed_text.is_empty() {
                return HashMap::new();
            }

            // 6. 为每个 Tool 消息生成压缩内容
            let mut result = HashMap::new();
            let tool_msg_indices: Vec<usize> = block
                .iter()
                .enumerate()
                .filter(|(_, m)| m.role == MessageRole::Tool)
                .map(|(i, _)| turn_start + i)
                .collect();

            // 所有 Tool 消息用同一个压缩结果替换
            for idx in tool_msg_indices {
                result.insert(idx, compressed_text.clone());
            }

            tracing::info!(
                "后台压缩完成: turn [{turn_start}..{turn_end}), 压缩后 {} 字符",
                compressed_text.chars().count()
            );
            result
        }
        Err(e) => {
            tracing::warn!("后台压缩 LLM 调用失败: {e}");
            HashMap::new()
        }
    }
}

// ─── 应用已压缩内容 ────────────────────────────────────────────────

/// 应用已压缩内容：替换 history 中的工具结果
///
/// 遍历 messages，将 pending 中对应索引的消息内容替换为压缩后的内容。
/// 只替换 Tool 角色的消息。
pub fn apply_pending(
    messages: &mut Vec<ConversationMessage>,
    pending: &HashMap<usize, String>,
) -> CompactionResult {
    let mut compacted_groups = 0usize;
    let mut chars_saved = 0usize;

    for (&idx, compressed) in pending.iter() {
        if idx >= messages.len() {
            continue;
        }
        let msg = &mut messages[idx];
        if msg.role != MessageRole::Tool {
            continue;
        }
        let old_len = msg.content.chars().count();
        let new_len = compressed.chars().count();
        if new_len < old_len {
            chars_saved += old_len - new_len;
            compacted_groups += 1;
            msg.content.clone_from(compressed);
        }
    }

    CompactionResult {
        compacted_groups,
        chars_saved,
    }
}

// ─── 辅助：计算 Tool 消息总字符数 ──────────────────────────────────

/// 计算消息列表中最近一轮（从最后一个 User 到末尾）的 Tool 消息总字符数
pub fn last_turn_tool_chars(messages: &[ConversationMessage]) -> usize {
    // 从后往前找到最后一个 User 消息的位置
    let last_user_idx = messages
        .iter()
        .rposition(|m| m.role == MessageRole::User);

    let start = match last_user_idx {
        Some(idx) => idx,
        None => 0,
    };

    messages[start..]
        .iter()
        .filter(|m| m.role == MessageRole::Tool)
        .map(|m| m.content.chars().count())
        .sum()
}

// ─── 测试 ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_turn_blocks_basic() {
        let messages = vec![
            ConversationMessage::user("问题1"),
            ConversationMessage::tool("工具结果1-1"),
            ConversationMessage::assistant("回答1"),
            ConversationMessage::user("问题2"),
            ConversationMessage::assistant("回答2"),
        ];

        let blocks = split_turn_blocks(&messages);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].start, 0);
        assert_eq!(blocks[0].end, 3);
        assert_eq!(blocks[0].tool_indices, vec![1]);
        assert_eq!(blocks[1].start, 3);
        assert_eq!(blocks[1].end, 5);
        assert!(blocks[1].tool_indices.is_empty());
    }

    #[test]
    fn find_slide_out_turn_returns_none_when_all_preserved() {
        let messages = vec![
            ConversationMessage::user("q1"),
            ConversationMessage::tool("t1"),
            ConversationMessage::assistant("a1"),
            ConversationMessage::user("q2"),
            ConversationMessage::assistant("a2"),
        ];
        let config = CompactionConfig {
            preserve_recent_turns: 4,
            ..Default::default()
        };
        let compressed = HashSet::new();
        assert_eq!(
            find_slide_out_turn(&messages, &compressed, &config),
            None
        );
    }

    #[test]
    fn find_slide_out_turn_finds_uncompressed() {
        // 6 个 turn blocks, preserve 4, 前两个中找未压缩的
        let mut messages = Vec::new();
        for i in 0..6 {
            messages.push(ConversationMessage::user(format!("q{i}")));
            messages.push(ConversationMessage::tool(format!("t{i}")));
            messages.push(ConversationMessage::assistant(format!("a{i}")));
        }

        let config = CompactionConfig {
            preserve_recent_turns: 4,
            ..Default::default()
        };
        let compressed = HashSet::new();

        // 应该找到第一个 turn block (索引 0..3)
        let result = find_slide_out_turn(&messages, &compressed, &config);
        assert!(result.is_some());
        let (start, end) = result.unwrap();
        assert_eq!(start, 0);
        assert_eq!(end, 3);
    }

    #[test]
    fn apply_pending_replaces_tool_messages() {
        let mut messages = vec![
            ConversationMessage::user("q1"),
            ConversationMessage::tool("很长的工具结果内容，这里有很多字符需要被压缩掉".repeat(10)),
            ConversationMessage::assistant("a1"),
        ];

        let compressed = "[工具调用结果已压缩] 结论：xxx".to_string();
        let mut pending = HashMap::new();
        pending.insert(1, compressed);

        let result = apply_pending(&mut messages, &pending);
        assert_eq!(result.compacted_groups, 1);
        assert!(result.chars_saved > 0);
        assert!(messages[1].content.starts_with("[工具调用结果已压缩]"));
    }

    #[test]
    fn apply_pending_skips_non_tool_messages() {
        let mut messages = vec![
            ConversationMessage::user("q1"),
            ConversationMessage::assistant("a1"),
        ];

        let compressed = "压缩内容".to_string();
        let mut pending = HashMap::new();
        pending.insert(1, compressed);

        let result = apply_pending(&mut messages, &pending);
        assert_eq!(result.compacted_groups, 0);
        assert_eq!(result.chars_saved, 0);
    }

    #[test]
    fn last_turn_tool_chars_counts_correctly() {
        let messages = vec![
            ConversationMessage::user("q1"),
            ConversationMessage::tool("tool1"),
            ConversationMessage::assistant("a1"),
            ConversationMessage::user("q2"),
            ConversationMessage::tool("tool2"),
            ConversationMessage::tool("tool3"),
        ];

        // 最后一轮(q2开始)有2个tool消息: "tool2"(5) + "tool3"(5) = 10
        let chars = last_turn_tool_chars(&messages);
        assert_eq!(chars, 10);
    }

    #[test]
    fn build_compressed_content_format() {
        let content = build_compressed_content(
            "排查代码 bug",
            "main → init → config.load",
            "  - src/main.rs（行 40-60，方法 run()）：缺少空指针检查",
            "config.load() 未处理空配置导致的 NPE",
        );
        assert!(content.starts_with("[工具调用结果已压缩]"));
        assert!(content.contains("排查代码 bug"));
        assert!(content.contains("config.load()"));
    }
}
