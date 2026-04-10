use brain_core::types::{RiskLevel, SafetyCheckResult, ToolCall};

/// 安全规则
#[derive(Debug, Clone)]
struct SafetyRule {
    /// 规则描述
    description: String,
    /// 匹配的关键词/模式
    patterns: Vec<String>,
    /// 匹配后的风险等级
    risk_level: RiskLevel,
    /// 是否需要用户授权
    requires_user_approval: bool,
}

/// 安全校验器
///
/// 基于规则的安全检查引擎。
/// 所有执行脑的操作必须经过此处校验。
pub struct SafetyChecker {
    rules: Vec<SafetyRule>,
}

impl SafetyChecker {
    pub fn new() -> Self {
        Self {
            rules: Self::builtin_rules(),
        }
    }

    /// 检查工具调用安全性
    pub fn check(&self, tool_call: &ToolCall) -> SafetyCheckResult {
        let tool_name = &tool_call.tool_name;
        let input_str = tool_call.input.to_string().to_lowercase();

        // 遍历规则，匹配最高风险等级
        let mut max_risk = RiskLevel::Low;
        let mut reasons: Vec<String> = Vec::new();
        let mut needs_approval = false;

        for rule in &self.rules {
            let matched = rule.patterns.iter().any(|p| {
                let p_lower = p.to_lowercase();
                // 工具名匹配或输入内容匹配
                tool_name.to_lowercase().contains(&p_lower) || input_str.contains(&p_lower)
            });

            if matched {
                if matches!(rule.risk_level, RiskLevel::High) {
                    max_risk = RiskLevel::High;
                    needs_approval = true;
                } else if matches!(rule.risk_level, RiskLevel::Medium)
                    && !matches!(max_risk, RiskLevel::High)
                {
                    max_risk = RiskLevel::Medium;
                }
                reasons.push(rule.description.clone());
                if rule.requires_user_approval {
                    needs_approval = true;
                }
            }
        }

        let safe = !matches!(max_risk, RiskLevel::High);
        let reason = if reasons.is_empty() {
            None
        } else {
            Some(reasons.join("; "))
        };

        SafetyCheckResult {
            safe,
            risk_level: max_risk,
            reason,
            requires_user_approval: needs_approval,
        }
    }

    /// 高危操作关键词列表（用于快思考快速判断）
    pub fn high_risk_keywords(&self) -> &[String] {
        static EMPTY: Vec<String> = Vec::new();
        // 返回第一个高风险规则的 patterns
        self.rules
            .iter()
            .find(|r| matches!(r.risk_level, RiskLevel::High))
            .map_or(&EMPTY, |r| &r.patterns)
    }

    /// 内置安全规则
    fn builtin_rules() -> Vec<SafetyRule> {
        vec![
            // === 高风险 ===
            SafetyRule {
                description: "危险删除命令".into(),
                patterns: vec![
                    "rm -rf".into(),
                    "rm -r".into(),
                    "rmdir /s".into(),
                    "del /f".into(),
                    "shutil.rmtree".into(),
                ],
                risk_level: RiskLevel::High,
                requires_user_approval: true,
            },
            SafetyRule {
                description: "数据库危险操作".into(),
                patterns: vec![
                    "drop table".into(),
                    "drop database".into(),
                    "truncate table".into(),
                    "delete from".into(),
                ],
                risk_level: RiskLevel::High,
                requires_user_approval: true,
            },
            SafetyRule {
                description: "Git 危险操作".into(),
                patterns: vec![
                    "git push --force".into(),
                    "git push -f".into(),
                    "git reset --hard".into(),
                    "git clean -f".into(),
                    "git checkout --".into(),
                ],
                risk_level: RiskLevel::High,
                requires_user_approval: true,
            },
            SafetyRule {
                description: "系统权限操作".into(),
                patterns: vec![
                    "chmod 777".into(),
                    "chown".into(),
                    "sudo".into(),
                    "mkfs".into(),
                    "dd if=".into(),
                ],
                risk_level: RiskLevel::High,
                requires_user_approval: true,
            },
            SafetyRule {
                description: "网络危险操作".into(),
                patterns: vec![
                    "curl | sh".into(),
                    "wget | sh".into(),
                    "nc -l".into(),
                ],
                risk_level: RiskLevel::High,
                requires_user_approval: true,
            },
            // === 中风险 ===
            SafetyRule {
                description: "文件修改操作".into(),
                patterns: vec![
                    "edit".into(),
                    "write".into(),
                ],
                risk_level: RiskLevel::Medium,
                requires_user_approval: false,
            },
            SafetyRule {
                description: "Shell 命令执行".into(),
                patterns: vec![
                    "bash".into(),
                    "sh -c".into(),
                    "exec".into(),
                ],
                risk_level: RiskLevel::Medium,
                requires_user_approval: false,
            },
            SafetyRule {
                description: "环境变量修改".into(),
                patterns: vec![
                    "export ".into(),
                    "setenv".into(),
                ],
                risk_level: RiskLevel::Medium,
                requires_user_approval: false,
            },
        ]
    }
}

impl Default for SafetyChecker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brain_core::types::ToolCall;

    fn make_tool_call(name: &str, input: &str) -> ToolCall {
        ToolCall {
            tool_name: name.into(),
            input: serde_json::Value::String(input.into()),
            validated: false,
            validation_id: None,
        }
    }

    #[test]
    fn low_risk_read_operation() {
        let checker = SafetyChecker::new();
        let call = make_tool_call("Read", "/path/to/file.txt");
        let result = checker.check(&call);
        assert!(result.safe);
        assert_eq!(result.risk_level, RiskLevel::Low);
        assert!(!result.requires_user_approval);
    }

    #[test]
    fn high_risk_rm_rf() {
        let checker = SafetyChecker::new();
        let call = make_tool_call("Bash", "rm -rf /tmp/test");
        let result = checker.check(&call);
        assert!(!result.safe);
        assert_eq!(result.risk_level, RiskLevel::High);
        assert!(result.requires_user_approval);
    }

    #[test]
    fn high_risk_drop_table() {
        let checker = SafetyChecker::new();
        let call = make_tool_call("Bash", "echo 'DROP TABLE users'");
        let result = checker.check(&call);
        assert!(!result.safe);
        assert!(result.reason.is_some());
    }

    #[test]
    fn high_risk_git_force_push() {
        let checker = SafetyChecker::new();
        let call = make_tool_call("Bash", "git push --force origin main");
        let result = checker.check(&call);
        assert!(!result.safe);
        assert!(result.requires_user_approval);
    }

    #[test]
    fn medium_risk_edit() {
        let checker = SafetyChecker::new();
        let call = make_tool_call("Edit", "modify source code");
        let result = checker.check(&call);
        assert!(result.safe); // 中风险不是不安全
        assert_eq!(result.risk_level, RiskLevel::Medium);
    }

    #[test]
    fn medium_risk_bash() {
        let checker = SafetyChecker::new();
        let call = make_tool_call("Bash", "ls -la");
        let result = checker.check(&call);
        // bash 本身是中风险
        assert_eq!(result.risk_level, RiskLevel::Medium);
    }

    #[test]
    fn safe_operation_no_reason() {
        let checker = SafetyChecker::new();
        let call = make_tool_call("Grep", "search pattern");
        let result = checker.check(&call);
        assert!(result.safe);
        assert!(result.reason.is_none());
    }
}
