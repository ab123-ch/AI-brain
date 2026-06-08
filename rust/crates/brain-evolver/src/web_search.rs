//! WebSearch — 进化脑联网搜索接口
//!
//! 提供 MCP 工具调用的抽象接口，用于 Phase 2 研究。
//! 实现可以是真实的 MCP 工具、Mock 或 Stub。

use crate::error::Result;

/// 搜索结果条目
#[derive(Clone, Debug)]
pub struct SearchResult {
    /// 标题
    pub title: String,
    /// URL
    pub url: String,
    /// 摘要内容
    pub snippet: String,
    /// 来源可信度 (0-100)
    pub credibility: u8,
}

/// 网页抓取结果
#[derive(Clone, Debug)]
pub struct PageContent {
    /// URL
    pub url: String,
    /// Markdown 正文
    pub content: String,
    /// 标题
    pub title: String,
}

/// 进化脑联网搜索接口
///
/// 实现可以是：
/// - 真实的 MCP 工具 (`mcp__firecrawl__search`, `mcp__web_reader`)
/// - Mock（用于测试）
/// - Stub（用于早期开发）
pub trait WebSearch: Send + Sync {
    /// 执行搜索
    ///
    /// `query`: 搜索关键词
    /// `max_results`: 最大结果数
    fn search(&self, query: &str, max_results: usize) -> Result<Vec<SearchResult>>;

    /// 抓取网页内容
    fn fetch_page(&self, url: &str) -> Result<PageContent>;

    /// 批量抓取
    fn fetch_pages(&self, urls: &[String]) -> Result<Vec<PageContent>> {
        urls.iter().map(|u| self.fetch_page(u)).collect()
    }
}

// ---------------------------------------------------------------------------
// Mock 实现（用于测试）
// ---------------------------------------------------------------------------

/// Mock WebSearch — 返回预设搜索结果
pub struct MockWebSearch {
    preset_results: Vec<SearchResult>,
    preset_page: Option<PageContent>,
}

impl MockWebSearch {
    pub fn new(results: Vec<SearchResult>) -> Self {
        Self {
            preset_results: results,
            preset_page: None,
        }
    }

    pub fn empty() -> Self {
        Self::new(vec![])
    }

    pub fn with_page(content: String) -> Self {
        Self {
            preset_results: vec![SearchResult {
                title: "Mock Result".into(),
                url: "https://example.com".into(),
                snippet: "Mock snippet".into(),
                credibility: 80,
            }],
            preset_page: Some(PageContent {
                url: "https://example.com".into(),
                content,
                title: "Mock Page".into(),
            }),
        }
    }

    pub fn rust_async_results() -> Self {
        Self::new(vec![
            SearchResult {
                title: "Rust Async Programming Guide".into(),
                url: "https://rust-lang.github.io/async-book/".into(),
                snippet: "Comprehensive guide to async Rust, covering futures, async/await, and executors.".into(),
                credibility: 95,
            },
            SearchResult {
                title: "Tokio Documentation".into(),
                url: "https://tokio.rs/tokio/tutorial".into(),
                snippet: "Tokio runtime tutorial: understanding async execution in Rust.".into(),
                credibility: 90,
            },
        ])
    }
}

impl WebSearch for MockWebSearch {
    fn search(&self, _query: &str, _max_results: usize) -> Result<Vec<SearchResult>> {
        Ok(self.preset_results.clone())
    }

    fn fetch_page(&self, _url: &str) -> Result<PageContent> {
        Ok(self.preset_page.clone().unwrap_or_else(|| PageContent {
            url: "https://example.com".into(),
            content: "# Mock Page\n\nThis is mock content for testing.".into(),
            title: "Mock".into(),
        }))
    }
}

// ---------------------------------------------------------------------------
// Stub 实现（早期开发用）
// ---------------------------------------------------------------------------

/// Stub WebSearch — 暂时返回空结果
pub struct StubWebSearch;

impl WebSearch for StubWebSearch {
    fn search(&self, _query: &str, _max_results: usize) -> Result<Vec<SearchResult>> {
        // Stub: 暂不实现真实搜索
        Ok(vec![])
    }

    fn fetch_page(&self, _url: &str) -> Result<PageContent> {
        // Stub: 暂不实现真实抓取
        Ok(PageContent {
            url: "stub".into(),
            content: String::new(),
            title: "Stub".into(),
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_web_search_empty() {
        let mock = MockWebSearch::empty();
        let results = mock.search("test", 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_mock_web_search_rust_async() {
        let mock = MockWebSearch::rust_async_results();
        let results = mock.search("rust async", 10).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].title.contains("Async"));
    }

    #[test]
    fn test_mock_web_search_with_page() {
        let mock = MockWebSearch::with_page("# Rust Async\n\nContent here.".into());
        let page = mock.fetch_page("https://example.com").unwrap();
        assert!(page.content.contains("Rust Async"));
    }

    #[test]
    fn test_stub_web_search() {
        let stub = StubWebSearch;
        let results = stub.search("test", 5).unwrap();
        assert!(results.is_empty());

        let page = stub.fetch_page("https://example.com").unwrap();
        assert!(page.content.is_empty());
    }

    #[test]
    fn test_batch_fetch() {
        let mock = MockWebSearch::with_page("content".into());
        let pages = mock.fetch_pages(&["url1".into(), "url2".into()]).unwrap();
        assert_eq!(pages.len(), 2);
    }
}