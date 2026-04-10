use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// 工具风险等级
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolRiskLevel {
    /// 低风险：读取类操作，自动放行
    Low,
    /// 中风险：修改类操作，需确认
    Medium,
    /// 高风险：删除/系统操作，必须用户授权
    High,
}

/// 工具能力描述
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCapability {
    /// 工具名称
    pub name: String,
    /// 工具描述
    pub description: String,
    /// 风险等级
    pub risk_level: ToolRiskLevel,
    /// 适用场景标签
    pub applicable_scenarios: Vec<String>,
    /// 是否需要校验脑审核
    pub requires_validation: bool,
}

/// 工具注册表
///
/// 管理所有可用工具的能力描述和风险等级。
/// 执行脑在执行前通过注册表判断工具是否需要校验。
pub struct ToolRegistry {
    tools: HashMap<String, ToolCapability>,
}

impl ToolRegistry {
    /// 创建一个包含内置工具的注册表
    pub fn with_builtin_tools() -> Self {
        let mut registry = Self {
            tools: HashMap::new(),
        };
        registry.register_builtin_tools();
        registry
    }

    /// 注册一个工具
    pub fn register(&mut self, capability: ToolCapability) {
        self.tools.insert(capability.name.clone(), capability);
    }

    /// 获取工具能力描述
    pub fn get(&self, name: &str) -> Option<&ToolCapability> {
        self.tools.get(name)
    }

    /// 获取工具的风险等级
    pub fn risk_level(&self, name: &str) -> ToolRiskLevel {
        self.tools
            .get(name)
            .map_or(ToolRiskLevel::High, |c| c.risk_level) // 未知工具默认高风险
    }

    /// 工具是否需要校验脑审核
    pub fn requires_validation(&self, name: &str) -> bool {
        self.tools
            .get(name)
            .is_some_and(|c| c.requires_validation)
    }

    /// 列出所有注册工具
    pub fn list(&self) -> Vec<&ToolCapability> {
        self.tools.values().collect()
    }

    /// 按场景搜索工具
    pub fn search_by_scenario(&self, scenario: &str) -> Vec<&ToolCapability> {
        let scenario_lower = scenario.to_lowercase();
        self.tools
            .values()
            .filter(|c| {
                c.applicable_scenarios
                    .iter()
                    .any(|s| s.to_lowercase().contains(&scenario_lower))
            })
            .collect()
    }

    /// 注册工具数
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    fn register_builtin_tools(&mut self) {
        // 低风险：读取类
        let low_risk = [
            ("Read", "读取文件内容", vec!["文件", "读取", "查看"]),
            ("Glob", "搜索文件路径", vec!["文件", "搜索", "查找"]),
            ("Grep", "搜索文件内容", vec!["搜索", "内容", "查找"]),
            ("LSP", "语言服务器代码智能", vec!["代码", "定义", "引用"]),
            ("WebSearch", "网络搜索", vec!["搜索", "网络", "信息"]),
        ];
        for (name, desc, scenarios) in low_risk {
            self.register(ToolCapability {
                name: name.into(),
                description: desc.into(),
                risk_level: ToolRiskLevel::Low,
                applicable_scenarios: scenarios.into_iter().map(String::from).collect(),
                requires_validation: false,
            });
        }

        // 中风险：修改类
        let medium_risk = [
            ("Edit", "编辑文件内容", vec!["文件", "编辑", "修改"]),
            ("Write", "创建/覆盖文件", vec!["文件", "创建", "写入"]),
            ("NotebookEdit", "编辑 Jupyter notebook", vec!["notebook", "编辑"]),
        ];
        for (name, desc, scenarios) in medium_risk {
            self.register(ToolCapability {
                name: name.into(),
                description: desc.into(),
                risk_level: ToolRiskLevel::Medium,
                applicable_scenarios: scenarios.into_iter().map(String::from).collect(),
                requires_validation: true,
            });
        }

        // 高风险：系统操作
        let high_risk = [
            ("Bash", "执行 shell 命令", vec!["命令", "执行", "系统"]),
        ];
        for (name, desc, scenarios) in high_risk {
            self.register(ToolCapability {
                name: name.into(),
                description: desc.into(),
                risk_level: ToolRiskLevel::High,
                applicable_scenarios: scenarios.into_iter().map(String::from).collect(),
                requires_validation: true,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_tools_registered() {
        let registry = ToolRegistry::with_builtin_tools();
        assert!(registry.len() >= 9); // Read, Glob, Grep, LSP, WebSearch, Edit, Write, NotebookEdit, Bash
    }

    #[test]
    fn risk_levels_correct() {
        let registry = ToolRegistry::with_builtin_tools();
        assert_eq!(registry.risk_level("Read"), ToolRiskLevel::Low);
        assert_eq!(registry.risk_level("Edit"), ToolRiskLevel::Medium);
        assert_eq!(registry.risk_level("Bash"), ToolRiskLevel::High);
        assert_eq!(registry.risk_level("Unknown"), ToolRiskLevel::High);
    }

    #[test]
    fn requires_validation() {
        let registry = ToolRegistry::with_builtin_tools();
        assert!(!registry.requires_validation("Read"));
        assert!(registry.requires_validation("Edit"));
        assert!(registry.requires_validation("Bash"));
    }

    #[test]
    fn search_by_scenario() {
        let registry = ToolRegistry::with_builtin_tools();
        let results = registry.search_by_scenario("文件");
        assert!(results.len() >= 3); // Read, Glob, Edit, Write
    }

    #[test]
    fn register_custom_tool() {
        let mut registry = ToolRegistry::with_builtin_tools();
        let initial_count = registry.len();
        registry.register(ToolCapability {
            name: "CustomTool".into(),
            description: "自定义工具".into(),
            risk_level: ToolRiskLevel::Low,
            applicable_scenarios: vec!["自定义".into()],
            requires_validation: false,
        });
        assert_eq!(registry.len(), initial_count + 1);
    }
}
