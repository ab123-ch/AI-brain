use std::collections::HashSet;

use brain_core::types::{PitfallCategory, PitfallRecord};

use crate::eval_brain::{EvalIssue, IssueCategory, IssueSeverity};

/// 检测偷懒行为
///
/// 模式包括：
/// - TODO / FIXME / HACK / XXX 注释（表示未完成）
/// - 省略号实现（`...`、`/* ... */` 占位）
/// - "未实现" / "略" / "省略" 等中文偷懒标记
pub(crate) fn detect_lazy_behavior(ai_output: &str) -> Vec<EvalIssue> {
    let mut issues = Vec::new();
    let lines: Vec<&str> = ai_output.lines().collect();

    // 按行检测 TODO/FIXME/HACK/XXX 模式
    for line in &lines {
        let trimmed_upper = line.trim().to_uppercase();
        let trimmed = line.trim();

        if trimmed_upper.contains("TODO:")
            || trimmed_upper.contains("TODO!")
            || trimmed_upper.contains("TODO ")
        {
            issues.push(EvalIssue {
                severity: IssueSeverity::Critical,
                category: IssueCategory::LazyBehavior,
                description: format!("检测到 TODO 标记: {trimmed}"),
                suggestion: "请直接实现完整逻辑，不要使用 TODO 占位".into(),
            });
        }

        if trimmed_upper.contains("FIXME:") || trimmed_upper.contains("FIXME!") {
            issues.push(EvalIssue {
                severity: IssueSeverity::Critical,
                category: IssueCategory::LazyBehavior,
                description: format!("检测到 FIXME 标记: {trimmed}"),
                suggestion: "请直接修复问题，不要使用 FIXME 占位".into(),
            });
        }

        if trimmed_upper.contains("HACK:") || trimmed_upper.contains("HACK!") {
            issues.push(EvalIssue {
                severity: IssueSeverity::Warning,
                category: IssueCategory::LazyBehavior,
                description: format!("检测到 HACK 标记: {trimmed}"),
                suggestion: "请使用正规方案替代 HACK".into(),
            });
        }
    }

    // 检测省略号实现模式
    let ellipsis_patterns = ["// ...", "# ...", "/* ... */", "<!-- ... -->"];
    for line in &lines {
        let trimmed = line.trim();
        for pattern in &ellipsis_patterns {
            if trimmed.contains(pattern) {
                issues.push(EvalIssue {
                    severity: IssueSeverity::Critical,
                    category: IssueCategory::LazyBehavior,
                    description: format!("检测到省略号占位: {trimmed}"),
                    suggestion: "请实现完整代码，不要使用省略号占位".into(),
                });
                break;
            }
        }
    }

    // 检测中文偷懒标记
    let lazy_chinese_patterns = [
        ("未实现", "请实现完整功能"),
        ("此处省略", "请补全完整内容"),
        ("略...", "请补全完整内容"),
        ("暂时先这样", "请确保实现完整"),
    ];

    for line in &lines {
        let trimmed = line.trim();
        for (pattern, suggestion) in &lazy_chinese_patterns {
            if trimmed.contains(pattern) {
                issues.push(EvalIssue {
                    severity: IssueSeverity::Warning,
                    category: IssueCategory::LazyBehavior,
                    description: format!("检测到偷懒标记 '{pattern}': {trimmed}"),
                    suggestion: (*suggestion).into(),
                });
            }
        }
    }

    // 去重：同一行可能触发多个模式
    deduplicate_issues(&mut issues);

    issues
}

/// 检测禁忌词违规
///
/// 在 AI 输出中搜索用户明确禁止的词汇或模式。
/// 采用大小写不敏感匹配。
pub(crate) fn detect_taboo_violations(ai_output: &str, taboos: &[String]) -> Vec<EvalIssue> {
    let mut issues = Vec::new();
    let output_lower = ai_output.to_lowercase();

    for taboo in taboos {
        if taboo.trim().is_empty() {
            continue;
        }
        let taboo_lower = taboo.to_lowercase();

        if output_lower.contains(&taboo_lower) {
            issues.push(EvalIssue {
                severity: IssueSeverity::Critical,
                category: IssueCategory::PreferenceViolation,
                description: format!("AI 输出包含禁忌词/模式: '{taboo}'"),
                suggestion: format!("请移除或替换 '{taboo}'，这是用户明确禁止的"),
            });
        }
    }

    deduplicate_issues(&mut issues);
    issues
}

/// 检测已踩坑模式复现
///
/// 基于踩坑记录的描述，使用关键词匹配检测 AI 输出是否重复了相同的错误模式。
/// 对每条踩坑记录提取关键词，在 AI 输出中进行匹配。
pub(crate) fn detect_pitfall_repeats(
    ai_output: &str,
    pitfalls: &[PitfallRecord],
) -> Vec<EvalIssue> {
    let mut issues = Vec::new();
    let output_lower = ai_output.to_lowercase();

    for pitfall in pitfalls {
        let keywords = extract_pitfall_keywords(&pitfall.description);

        // 至少匹配 2 个关键词才算复现（避免单个通用词误报）
        let matched_count = keywords
            .iter()
            .filter(|kw| output_lower.contains(&kw.to_lowercase()))
            .count();

        let min_match = if keywords.len() <= 2 {
            keywords.len()
        } else {
            2
        };

        if matched_count >= min_match && !keywords.is_empty() {
            let severity = match pitfall.category {
                PitfallCategory::LazyBehavior | PitfallCategory::WrongAnswer => {
                    IssueSeverity::Critical
                }
                PitfallCategory::ToolFailure
                | PitfallCategory::FormatIssue
                | PitfallCategory::Other => IssueSeverity::Warning,
            };

            let suggestion = if let Some(ref correction) = pitfall.user_correction {
                format!("用户曾纠正: {correction}")
            } else {
                "请检查是否重复了已知的错误模式".into()
            };

            issues.push(EvalIssue {
                severity,
                category: IssueCategory::PitfallRepeat,
                description: format!(
                    "疑似重复踩坑 [出现{}次]: {}",
                    pitfall.occurrence_count, pitfall.description
                ),
                suggestion,
            });
        }
    }

    deduplicate_issues(&mut issues);
    issues
}

/// 从踩坑描述中提取搜索关键词
///
/// 策略：
/// 1. 按标点、空格、括号分词
/// 2. 对每个片段：含 ASCII 的保留原样并过滤停用词；纯中文的保留原片段
/// 3. 过滤过短的词（< 2 字符 / < 2 Unicode 字符）
fn extract_pitfall_keywords(description: &str) -> Vec<String> {
    let separators = [
        ' ', ',', '，', '。', '、', '；', '；', ':', '：', '!', '！', '?', '？', '\n', '\t', '(',
        ')', '（', '）', '[', ']', '{', '}',
    ];

    let stopwords = [
        "的", "了", "是", "在", "有", "和", "就", "不", "也", "都", "这", "那", "被", "把", "会",
        "要", "可以", "一个", "the", "is", "a", "an", "it", "to", "of", "in", "for",
    ];

    description
        .split(|c: char| separators.contains(&c))
        .map(|s| s.trim().to_string())
        .filter(|s| {
            let char_count = s.chars().count();
            char_count >= 2 && !stopwords.contains(&s.as_str())
        })
        .collect()
}

/// 按 description 前缀去重
fn deduplicate_issues(issues: &mut Vec<EvalIssue>) {
    let mut seen = HashSet::new();
    issues.retain(|issue| {
        let key = format!(
            "{:?}:{:?}:{}",
            issue.severity,
            issue.category,
            issue.description.chars().take(60).collect::<String>()
        );
        seen.insert(key)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::PitfallCategory;
    use chrono::Utc;

    fn make_pitfall(desc: &str, category: PitfallCategory) -> PitfallRecord {
        PitfallRecord {
            id: "p1".into(),
            category,
            description: desc.into(),
            user_correction: None,
            occurred_at: Utc::now(),
            occurrence_count: 1,
        }
    }

    // -- 偷懒行为检测 --

    #[test]
    fn detect_todo_in_code() {
        let issues = detect_lazy_behavior("fn main() {\n    // TODO: implement this\n}");
        assert!(!issues.is_empty());
        assert_eq!(issues[0].category, IssueCategory::LazyBehavior);
        assert_eq!(issues[0].severity, IssueSeverity::Critical);
        assert!(issues[0].description.contains("TODO"));
    }

    #[test]
    fn detect_fixme_in_code() {
        let issues = detect_lazy_behavior("// FIXME! this is broken");
        assert!(!issues.is_empty());
        assert!(issues[0].description.contains("FIXME"));
    }

    #[test]
    fn detect_hack_in_code() {
        let issues = detect_lazy_behavior("// HACK: temporary workaround");
        assert!(!issues.is_empty());
        assert!(issues[0].description.contains("HACK"));
        assert_eq!(issues[0].severity, IssueSeverity::Warning);
    }

    #[test]
    fn detect_ellipsis_placeholder() {
        let issues = detect_lazy_behavior("fn process() {\n    // ...\n}");
        assert!(!issues.is_empty());
        assert!(issues[0].description.contains("省略号"));
    }

    #[test]
    fn detect_chinese_lazy_markers() {
        let issues = detect_lazy_behavior("这个功能未实现，后续再补");
        assert!(!issues.is_empty());
        assert!(issues[0].description.contains("未实现"));
    }

    #[test]
    fn no_false_positive_on_normal_code() {
        let issues = detect_lazy_behavior(
            "fn main() {\n    println!(\"Hello, world!\");\n    let result = compute(42);\n}",
        );
        assert!(issues.is_empty());
    }

    // -- 禁忌词检测 --

    #[test]
    fn detect_taboo_exact_match() {
        let taboos = vec!["password".into(), "secret_key".into()];
        let issues = detect_taboo_violations("请使用 password 字段存储密码", &taboos);
        assert!(!issues.is_empty());
        assert_eq!(issues[0].category, IssueCategory::PreferenceViolation);
        assert_eq!(issues[0].severity, IssueSeverity::Critical);
    }

    #[test]
    fn taboo_case_insensitive() {
        let taboos = vec!["Password".into()];
        let issues = detect_taboo_violations("use PASSWORD field", &taboos);
        assert!(!issues.is_empty());
    }

    #[test]
    fn taboo_not_found() {
        let taboos = vec!["forbidden_word".into()];
        let issues = detect_taboo_violations("这段代码没有问题", &taboos);
        assert!(issues.is_empty());
    }

    #[test]
    fn taboo_empty_string_skipped() {
        let taboos = vec![String::new(), "  ".into()];
        let issues = detect_taboo_violations("anything goes", &taboos);
        assert!(issues.is_empty());
    }

    // -- 踩坑复现检测 --

    #[test]
    fn detect_pitfall_repeat_with_enough_keywords() {
        let pitfall = make_pitfall(
            "使用了 unwrap() 导致 panic 崩溃",
            PitfallCategory::WrongAnswer,
        );
        let issues = detect_pitfall_repeats("这里直接调用 unwrap() 会 panic", &[pitfall]);
        assert!(!issues.is_empty());
        assert_eq!(issues[0].category, IssueCategory::PitfallRepeat);
    }

    #[test]
    fn pitfall_no_repeat_when_keywords_dont_match() {
        let pitfall = make_pitfall("数据库连接超时未重试", PitfallCategory::WrongAnswer);
        let issues = detect_pitfall_repeats("这段代码使用安全的错误处理方式", &[pitfall]);
        assert!(issues.is_empty());
    }

    #[test]
    fn pitfall_with_user_correction() {
        let mut pitfall = make_pitfall(
            "使用了 unwrap() 导致 panic 崩溃",
            PitfallCategory::ToolFailure,
        );
        pitfall.user_correction = Some("应该使用 ok_or_else 处理错误".into());
        let issues = detect_pitfall_repeats("直接 unwrap() 会 panic", &[pitfall]);
        assert!(!issues.is_empty());
        assert!(issues[0].suggestion.contains("用户曾纠正"));
    }

    #[test]
    fn pitfall_empty_list_no_issues() {
        let issues = detect_pitfall_repeats("任何内容", &[]);
        assert!(issues.is_empty());
    }

    // -- 关键词提取 --

    #[test]
    fn extract_keywords_filters_stopwords() {
        let keywords = extract_pitfall_keywords("这是一个错误，的实现方式，Rust");
        // 停用词 "的" 被过滤
        assert!(
            !keywords.iter().any(|k| k == "的"),
            "stopword should be filtered"
        );
        // "Rust" 保留原样（>= 2 字符且不是停用词）
        assert!(keywords.contains(&"Rust".to_string()));
        // "这是一个错误" 保留为完整片段
        assert!(keywords.contains(&"这是一个错误".to_string()));
    }

    #[test]
    fn extract_keywords_filters_short_words() {
        let keywords = extract_pitfall_keywords("a b cc ddd");
        assert!(!keywords.contains(&"a".to_string()));
        assert!(!keywords.contains(&"b".to_string()));
        assert!(keywords.contains(&"cc".to_string()));
        assert!(keywords.contains(&"ddd".to_string()));
    }

    // -- 去重 --

    #[test]
    fn deduplicate_removes_duplicates() {
        let mut issues = vec![
            EvalIssue {
                severity: IssueSeverity::Critical,
                category: IssueCategory::LazyBehavior,
                description: "TODO: implement".into(),
                suggestion: "implement it".into(),
            },
            EvalIssue {
                severity: IssueSeverity::Critical,
                category: IssueCategory::LazyBehavior,
                description: "TODO: implement".into(),
                suggestion: "implement it now".into(),
            },
        ];
        deduplicate_issues(&mut issues);
        assert_eq!(issues.len(), 1);
    }
}
