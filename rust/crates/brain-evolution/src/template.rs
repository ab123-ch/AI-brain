use brain_core::types::BrainId;
use serde::{Deserialize, Serialize};

/// 副脑模板 — 新副脑的"基因"
///
/// 定义一个副脑的所有可配置属性，用于动态创建新副脑。
/// 模板可以从磁盘加载，也可以通过 API/CLI 动态注册。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[must_use]
pub struct BrainTemplate {
    /// 副脑名称（唯一标识），如 "code-review"
    pub name: String,
    /// 能力描述
    pub description: String,
    /// system prompt 模板
    pub prompt_template: String,
    /// 快思考规则（关键词匹配模式）
    pub fast_think_rules: Vec<FastThinkRule>,
    /// 初始权重，默认 0.5
    pub initial_weight: f64,
    /// 从哪个副脑分裂而来（可选）
    pub parent_brain: Option<String>,
    /// 能力标签，如 `code_review`, `security`
    pub capabilities: Vec<String>,
}

/// 快思考规则 — 关键词匹配
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FastThinkRule {
    /// 匹配关键词列表（任一匹配即命中）
    pub keywords: Vec<String>,
    /// 命中后的置信度
    pub confidence: f64,
    /// 命中后的摘要模板
    pub summary_template: String,
}

impl BrainTemplate {
    /// 创建一个新的模板（使用默认值）
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            prompt_template: String::new(),
            fast_think_rules: Vec::new(),
            initial_weight: 0.5,
            parent_brain: None,
            capabilities: Vec::new(),
        }
    }

    /// 设置 prompt 模板
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt_template = prompt.into();
        self
    }

    /// 添加快思考规则
    pub fn with_rule(mut self, rule: FastThinkRule) -> Self {
        self.fast_think_rules.push(rule);
        self
    }

    /// 设置初始权重
    pub fn with_weight(mut self, weight: f64) -> Self {
        self.initial_weight = weight.clamp(0.1, 1.0);
        self
    }

    /// 设置父副脑
    pub fn with_parent(mut self, parent: impl Into<String>) -> Self {
        self.parent_brain = Some(parent.into());
        self
    }

    /// 添加能力标签
    pub fn with_capabilities(mut self, caps: Vec<&str>) -> Self {
        self.capabilities = caps.into_iter().map(String::from).collect();
        self
    }

    /// 生成 BrainId
    pub fn brain_id(&self) -> BrainId {
        BrainId(self.name.clone())
    }

    /// 序列化为 TOML 字符串
    pub fn to_toml(&self) -> Result<String, super::EvolutionError> {
        toml::to_string_pretty(self)
            .map_err(|e| super::EvolutionError::PersistenceFailed(e.to_string()))
    }

    /// 从 TOML 字符串反序列化
    pub fn from_toml(s: &str) -> Result<Self, super::EvolutionError> {
        toml::from_str(s).map_err(|e| super::EvolutionError::LoadFailed(e.to_string()))
    }
}

/// 内置模板：代码审查副脑
pub fn builtin_code_review_template() -> BrainTemplate {
    BrainTemplate::new(
        "code-review",
        "代码审查专用副脑，检测安全漏洞、代码风格和最佳实践",
    )
    .with_prompt("你是一个代码审查专家。分析代码中的安全问题、性能瓶颈和改进机会。")
    .with_rule(FastThinkRule {
        keywords: vec![
            "代码审查".into(),
            "code review".into(),
            "安全漏洞".into(),
            "代码质量".into(),
        ],
        confidence: 0.85,
        summary_template: "检测到代码审查请求，准备分析代码质量".into(),
    })
    .with_capabilities(vec!["code_review", "security", "best_practices"])
}

/// 内置模板：文档生成副脑
pub fn builtin_docs_template() -> BrainTemplate {
    BrainTemplate::new("docs-gen", "文档生成副脑，根据代码自动生成文档和注释")
        .with_prompt("你是一个技术文档专家。根据代码和需求生成清晰的文档。")
        .with_rule(FastThinkRule {
            keywords: vec![
                "生成文档".into(),
                "写文档".into(),
                "generate docs".into(),
                "README".into(),
            ],
            confidence: 0.8,
            summary_template: "检测到文档生成请求".into(),
        })
        .with_capabilities(vec!["documentation", "markdown"])
}

/// 获取所有内置模板
pub fn builtin_templates() -> Vec<BrainTemplate> {
    vec![builtin_code_review_template(), builtin_docs_template()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_template_new() {
        let t = BrainTemplate::new("test", "test desc");
        assert_eq!(t.name, "test");
        assert_eq!(t.description, "test desc");
        assert!(t.fast_think_rules.is_empty());
        assert!((t.initial_weight - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_template_builder() {
        let t = BrainTemplate::new("test", "desc")
            .with_weight(0.8)
            .with_parent("reasoning")
            .with_capabilities(vec!["a", "b"]);

        assert!((t.initial_weight - 0.8).abs() < f64::EPSILON);
        assert_eq!(t.parent_brain.as_deref(), Some("reasoning"));
        assert_eq!(t.capabilities, vec!["a", "b"]);
    }

    #[test]
    fn test_template_weight_clamp() {
        let t = BrainTemplate::new("test", "desc").with_weight(2.0);
        assert!((t.initial_weight - 1.0).abs() < f64::EPSILON);

        let t = BrainTemplate::new("test", "desc").with_weight(-1.0);
        assert!((t.initial_weight - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn test_template_brain_id() {
        let t = BrainTemplate::new("my-brain", "desc");
        assert_eq!(t.brain_id().to_string(), "my-brain");
    }

    #[test]
    fn test_toml_roundtrip() {
        let t = BrainTemplate::new("test", "desc")
            .with_prompt("test prompt")
            .with_rule(FastThinkRule {
                keywords: vec!["hello".into()],
                confidence: 0.9,
                summary_template: "hello detected".into(),
            })
            .with_capabilities(vec!["test"]);

        let toml_str = t.to_toml().unwrap();
        let loaded = BrainTemplate::from_toml(&toml_str).unwrap();
        assert_eq!(loaded.name, "test");
        assert_eq!(loaded.prompt_template, "test prompt");
        assert_eq!(loaded.fast_think_rules.len(), 1);
        assert_eq!(loaded.capabilities, vec!["test"]);
    }

    #[test]
    fn test_builtin_templates() {
        let templates = builtin_templates();
        assert!(!templates.is_empty());
        for t in &templates {
            assert!(!t.name.is_empty());
            assert!(!t.capabilities.is_empty());
        }
    }
}
