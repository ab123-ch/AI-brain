use brain_core::types::BroadcastMessage;

use crate::experience::ExperienceStore;

/// 快思考模式匹配器
///
/// 从广播消息中提取关键词，匹配经验库中的 trigger_pattern。
/// 延迟目标: ~10ms
pub struct PatternMatcher {
    experience: ExperienceStore,
}

impl PatternMatcher {
    pub fn new(experience: ExperienceStore) -> Self {
        Self { experience }
    }

    /// 快速匹配，返回最佳匹配经验的推理路径
    ///
    /// 返回 (confidence, reasoning_path, experience_id)
    pub fn match_pattern(&self, msg: &BroadcastMessage) -> Option<(f64, Vec<String>, String)> {
        let keywords = extract_keywords_from_message(msg);
        if keywords.is_empty() {
            return None;
        }

        let results = self.experience.search(&keywords, 1);
        results.first().map(|entry| {
            (
                entry.success_rate * 0.9, // 经验匹配的置信度折扣
                entry.reasoning_path.clone(),
                entry.id.clone(),
            )
        })
    }

    /// 快速检查是否有相关经验
    pub fn has_match(&self, msg: &BroadcastMessage) -> bool {
        let keywords = extract_keywords_from_message(msg);
        if keywords.is_empty() {
            return false;
        }
        self.experience.has_match(&keywords)
    }

    /// 获取反面案例（供慢思考参考）
    pub fn get_negative_examples(&self, msg: &BroadcastMessage, limit: usize) -> Vec<String> {
        let keywords = extract_keywords_from_message(msg);
        self.experience
            .get_negative_examples(&keywords, limit)
            .iter()
            .filter_map(|e| {
                e.failure_reason
                    .as_ref()
                    .map(|r| format!("失败模式[{}]: {}", e.trigger_pattern, r))
            })
            .collect()
    }

    /// 访问底层经验库（用于写入新经验）
    pub fn experience_mut(&mut self) -> &mut ExperienceStore {
        &mut self.experience
    }

    pub fn experience(&self) -> &ExperienceStore {
        &self.experience
    }
}

/// 从广播消息中提取关键词
fn extract_keywords_from_message(msg: &BroadcastMessage) -> Vec<String> {
    let mut keywords = Vec::new();

    // 从 content 中提取（按标点分割）
    let content_words: Vec<String> = msg
        .content
        .split(
            &[
                ' ', ',', '，', '。', '、', '；', '！', '？', '\n', '\t', ':', '：',
            ][..],
        )
        .flat_map(extract_subwords)
        .filter(|s| s.len() >= 2)
        .take(12)
        .collect();
    keywords.extend(content_words);

    // 从 raw_input 中补充
    let raw_words: Vec<String> = msg
        .raw_input
        .split(&[' ', ',', '，', '。', '、', '；', '！', '？', '\n', '\t'][..])
        .flat_map(extract_subwords)
        .filter(|s| s.len() >= 2)
        .take(6)
        .collect();
    keywords.extend(raw_words);

    keywords.dedup();
    keywords
}

/// 从一个文本段中提取子词（支持中文 bigram）
fn extract_subwords(segment: &str) -> Vec<String> {
    let trimmed = segment.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    // 如果包含 ASCII 字母/数字，直接返回整个段
    if trimmed.chars().any(|c| c.is_ascii_alphanumeric()) {
        return vec![trimmed.to_string()];
    }

    // 纯 CJK 字符：生成 2-gram 和 3-gram
    let chars: Vec<char> = trimmed.chars().collect();
    let mut ngrams = Vec::new();

    if chars.len() >= 2 {
        // 整段也作为一个关键词
        ngrams.push(trimmed.to_string());
    }

    // 2-gram
    for window in chars.windows(2) {
        let gram: String = window.iter().collect();
        ngrams.push(gram);
    }

    // 3-gram
    if chars.len() >= 3 {
        for window in chars.windows(3) {
            let gram: String = window.iter().collect();
            ngrams.push(gram);
        }
    }

    ngrams
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::experience::ExperienceEntry;
    use brain_core::types::BrainContext;
    use chrono::Utc;
    use tempfile::TempDir;

    fn make_matcher() -> PatternMatcher {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("exp.json");
        let store = ExperienceStore::new(path, 0.5);
        std::mem::forget(tmp);
        PatternMatcher::new(store)
    }

    fn make_broadcast(content: &str) -> BroadcastMessage {
        BroadcastMessage {
            content: content.into(),
            raw_input: content.into(),
            context: BrainContext {
                current_date: "2026-04-04".into(),
                cwd: "/test".into(),
                git_branch: Some("main".into()),
                platform: "darwin".into(),
            },
            timestamp: Utc::now(),
        }
    }

    fn make_entry(id: &str, pattern: &str, rate: f64) -> ExperienceEntry {
        ExperienceEntry {
            id: id.into(),
            trigger_pattern: pattern.into(),
            reasoning_path: vec!["步骤1".into(), "步骤2".into()],
            tools_used: vec![],
            success_rate: rate,
            usage_count: 1,
            is_negative: false,
            files_modified: vec![],
            failure_reason: None,
            created_at: Utc::now(),
            last_used: Utc::now(),
        }
    }

    #[test]
    fn no_match_when_empty() {
        let matcher = make_matcher();
        let msg = make_broadcast("测试查询");
        assert!(!matcher.has_match(&msg));
        assert!(matcher.match_pattern(&msg).is_none());
    }

    #[test]
    fn match_existing_experience() {
        let mut matcher = make_matcher();
        matcher
            .experience_mut()
            .store(make_entry("exp-001", "代码,bug修复", 0.9));

        let msg = make_broadcast("需要修复代码中的bug");
        assert!(matcher.has_match(&msg));

        let (confidence, path, id) = matcher.match_pattern(&msg).unwrap();
        assert_eq!(id, "exp-001");
        assert!(confidence > 0.5);
        assert_eq!(path.len(), 2);
    }

    #[test]
    fn get_negative_examples_works() {
        let mut matcher = make_matcher();
        let mut neg = make_entry("exp-neg", "代码,重构", 0.3);
        neg.is_negative = true;
        neg.failure_reason = Some("重构范围过大".into());
        matcher.experience_mut().store(neg);

        let msg = make_broadcast("代码重构任务");
        let examples = matcher.get_negative_examples(&msg, 5);
        assert_eq!(examples.len(), 1);
        assert!(examples[0].contains("重构范围过大"));
    }
}
