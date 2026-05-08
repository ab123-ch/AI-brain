//! 智能补全模块
//!
//! 集成 brain-evolution 的 SuggestionEngine/BrainRegistry，
//! 为 TUI 输入提供上下文感知的 Tab 补全和提示。
//!
//! 补全来源：
//! 1. 内置命令（:help, :status 等）
//! 2. brain-evolution 模板名（BrainRegistry）
//! 3. 进化任务模式关键词（SuggestionEngine）
//! 4. 历史输入补全

/// 补全候选项
#[derive(Debug, Clone)]
pub struct CompletionItem {
    /// 显示的文本
    pub display: String,
    /// 替换后文本（可能比 display 长，例如补全整个命令）
    pub replacement: String,
    /// 分类标签，用于分组显示
    pub category: CompletionCategory,
    /// 描述/帮助文本
    pub description: Option<String>,
}

/// 补全分类
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionCategory {
    /// / 开头的命令
    Command,
    /// 副脑模板名
    BrainTemplate,
    /// 关键词模式
    Keyword,
    /// 历史输入
    History,
    /// 动词/动作
    Action,
    /// 其他
    Other,
}

impl CompletionCategory {
    pub fn label(&self) -> &'static str {
        match self {
            CompletionCategory::Command => "命令",
            CompletionCategory::BrainTemplate => "副脑",
            CompletionCategory::Keyword => "模式",
            CompletionCategory::History => "历史",
            CompletionCategory::Action => "动作",
            CompletionCategory::Other => "",
        }
    }
}

/// 当前输入的上下文分析结果
#[derive(Debug, Clone)]
pub struct InputContext {
    /// 原始输入文本
    pub text: String,
    /// 光标位置（字符偏移）
    pub cursor_pos: usize,
    /// 光标前的词（用于补全匹配）
    pub prefix: String,
    /// 是否在命令模式（以 : 开头）
    pub is_command: bool,
    /// 命令名（如果是命令模式）
    pub command_name: Option<String>,
    /// 命令参数（如果是命令模式且有参数）
    pub command_args: Option<String>,
}

impl InputContext {
    /// 从当前输入和光标位置分析上下文
    pub fn analyze(text: &str, cursor_pos: usize) -> Self {
        let text_len = text.len();
        let pos = cursor_pos.min(text_len);

        // 截取光标前的部分
        let before_cursor = &text[..pos];

        // 判断是否命令模式
        let trimmed = text.trim_start();
        let is_command = trimmed.starts_with(':');

        // 获取光标前的词
        let prefix = before_cursor
            .split_whitespace()
            .last()
            .unwrap_or("")
            .to_string();

        // 解析命令
        let (command_name, command_args) = if is_command {
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            let name = parts.first().map(|s| s.to_string());
            let args = parts.get(1).map(|s| s.to_string());
            (name, args)
        } else {
            (None, None)
        };

        Self {
            text: text.to_string(),
            cursor_pos: pos,
            prefix,
            is_command,
            command_name,
            command_args,
        }
    }
}

/// 进化感知补全器
///
/// 集成 brain-evolution 的 SuggestionEngine 和 BrainRegistry，
/// 提供上下文感知的补全建议。
pub struct EvolutionCompleter {
    /// 内置命令列表
    builtin_commands: Vec<BuiltinCommand>,
    /// 进化模板名缓存（来自 BrainRegistry）
    template_names: Vec<String>,
    /// 任务模式关键词（来自 SuggestionEngine）
    pattern_keywords: Vec<String>,
}

/// 内置命令定义
#[derive(Debug, Clone)]
pub struct BuiltinCommand {
    pub name: String,
    pub args_hint: Option<String>,
    pub description: String,
}

impl EvolutionCompleter {
    /// 创建补全器，从 brain-evolution 组件加载数据
    pub fn new(template_names: Vec<String>, pattern_keywords: Vec<String>) -> Self {
        Self {
            builtin_commands: Self::default_commands(),
            template_names,
            pattern_keywords,
        }
    }

    /// 无 brain-evolution 的轻量版本
    pub fn lightweight() -> Self {
        Self {
            builtin_commands: Self::default_commands(),
            template_names: Vec::new(),
            pattern_keywords: Vec::new(),
        }
    }

    /// 从 Registry 和 SuggestionEngine 更新补全数据
    pub fn with_data(template_names: Vec<String>, pattern_keywords: Vec<String>) -> Self {
        Self::new(template_names, pattern_keywords)
    }

    /// 获取所有模板名（供外部使用）
    pub fn template_names(&self) -> &[String] {
        &self.template_names
    }

    // ─── 补全逻辑 ──────────────────────────────────────────────────

    /// 根据上下文获取补全建议
    pub fn complete(&self, ctx: &InputContext, history: &[String]) -> Vec<CompletionItem> {
        if ctx.is_command {
            return self.complete_command(ctx);
        }
        self.complete_normal(ctx, history)
    }

    /// 补全命令模式（:xxx）
    fn complete_command(&self, ctx: &InputContext) -> Vec<CompletionItem> {
        let mut items = Vec::new();

        // 获取命令名部分（去掉 :）
        let cmd_prefix = ctx
            .command_name
            .as_deref()
            .unwrap_or("")
            .trim_start_matches(':');

        // 匹配内置命令
        for cmd in &self.builtin_commands {
            let cmd_full = format!(":{}", cmd.name);
            if cmd_full.starts_with(ctx.text.trim()) || cmd.name.starts_with(cmd_prefix) {
                let replacement = if cmd.args_hint.is_some() {
                    format!(":{} ", cmd.name)
                } else {
                    cmd_full.clone()
                };
                items.push(CompletionItem {
                    display: cmd_full,
                    replacement,
                    category: CompletionCategory::Command,
                    description: Some(cmd.description.clone()),
                });
            }
        }

        // 匹配模板名（作为 :evo 的参数提示）
        if ctx
            .command_name
            .as_deref()
            .map_or(false, |n| n == ":evo" || n.starts_with(":evo "))
        {
            let args = ctx.command_args.as_deref().unwrap_or("");
            for name in &self.template_names {
                if name.starts_with(args) || args.is_empty() {
                    items.push(CompletionItem {
                        display: format!(":evo {name}"),
                        replacement: format!(":evo {name}"),
                        category: CompletionCategory::BrainTemplate,
                        description: Some(format!("从模板 `{name}` 创建副脑")),
                    });
                }
            }
        }

        items
    }

    /// 补全普通输入模式（非命令）
    fn complete_normal(&self, ctx: &InputContext, history: &[String]) -> Vec<CompletionItem> {
        let mut items = Vec::new();
        let prefix = ctx.prefix.to_lowercase();

        if prefix.is_empty() {
            return items;
        }

        // 1. 补全模板名关键词
        for name in &self.template_names {
            if name.to_lowercase().contains(&prefix) {
                items.push(CompletionItem {
                    display: name.clone(),
                    replacement: name.clone(),
                    category: CompletionCategory::BrainTemplate,
                    description: Some("副脑模板 — 可用 :evo <模板名> 创建".to_string()),
                });
            }
        }

        // 2. 补全任务模式关键词
        for kw in &self.pattern_keywords {
            if kw.to_lowercase().contains(&prefix) {
                items.push(CompletionItem {
                    display: kw.clone(),
                    replacement: kw.clone(),
                    category: CompletionCategory::Keyword,
                    description: Some("任务模式关键词".to_string()),
                });
            }
        }

        // 3. 补全历史输入
        for h in history.iter().rev() {
            if h.to_lowercase().contains(&prefix) && !items.iter().any(|i| i.display == *h) {
                items.push(CompletionItem {
                    display: h.clone(),
                    replacement: h.clone(),
                    category: CompletionCategory::History,
                    description: None,
                });
            }
        }

        items
    }

    /// 获取当前上下文下的提示信息（状态栏/底部显示）
    pub fn hint(&self, ctx: &InputContext) -> Option<String> {
        if ctx.is_command {
            let name = ctx.command_name.as_deref()?;
            for cmd in &self.builtin_commands {
                let full = format!(":{}", cmd.name);
                if full == name || format!(":{} ", cmd.name) == name {
                    return Some(format!(":{} — {}", cmd.name, cmd.description));
                }
            }
            // 检查是否是已知命令的参数模式
            if let Some(args) = &ctx.command_args {
                let base_cmd = ctx.command_name.as_deref().unwrap_or("");
                if base_cmd == ":evo" && !args.is_empty() {
                    if self.template_names.iter().any(|t| t == args) {
                        return Some(format!("按 Enter 从模板 `{args}` 创建副脑"));
                    }
                }
            }
        }
        None
    }

    // ─── 默认命令集 ──────────────────────────────────────────────

    fn default_commands() -> Vec<BuiltinCommand> {
        vec![
            BuiltinCommand {
                name: "help".into(),
                args_hint: None,
                description: "显示帮助信息".into(),
            },
            BuiltinCommand {
                name: "status".into(),
                args_hint: None,
                description: "显示系统状态".into(),
            },
            BuiltinCommand {
                name: "memory".into(),
                args_hint: None,
                description: "显示记忆统计".into(),
            },
            BuiltinCommand {
                name: "evo".into(),
                args_hint: Some("<目标>".into()),
                description: "启动进化任务".into(),
            },
            BuiltinCommand {
                name: "evo-status".into(),
                args_hint: None,
                description: "查看进化状态".into(),
            },
            BuiltinCommand {
                name: "evo-approve".into(),
                args_hint: None,
                description: "确认合并进化结果".into(),
            },
            BuiltinCommand {
                name: "evo-reject".into(),
                args_hint: None,
                description: "拒绝并回滚进化".into(),
            },
            BuiltinCommand {
                name: "evo-diff".into(),
                args_hint: None,
                description: "查看进化变更".into(),
            },
            BuiltinCommand {
                name: "quit".into(),
                args_hint: None,
                description: "退出并保存".into(),
            },
            BuiltinCommand {
                name: "exit".into(),
                args_hint: None,
                description: "退出".into(),
            },
        ]
    }

    /// 获取所有命令的显示列表（用于帮助/提示）
    pub fn all_commands(&self) -> Vec<&BuiltinCommand> {
        self.builtin_commands.iter().collect()
    }
}

/// 补全弹出窗口数据
pub struct CompletionPopup {
    /// 当前补全项列表
    pub items: Vec<CompletionItem>,
    /// 当前选中的索引
    pub selected: usize,
    /// 是否可见
    pub visible: bool,
}

impl CompletionPopup {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            selected: 0,
            visible: false,
        }
    }

    /// 设置新补全列表，重置选中
    pub fn set_items(&mut self, items: Vec<CompletionItem>) {
        self.visible = !items.is_empty();
        self.items = items;
        self.selected = 0;
    }

    /// 清除补全
    pub fn clear(&mut self) {
        self.items.clear();
        self.selected = 0;
        self.visible = false;
    }

    /// 选择下一个
    pub fn select_next(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + 1) % self.items.len();
        }
    }

    /// 选择上一个
    pub fn select_prev(&mut self) {
        if !self.items.is_empty() {
            self.selected = if self.selected == 0 {
                self.items.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    /// 获取当前选中项
    pub fn current(&self) -> Option<&CompletionItem> {
        if self.items.is_empty() {
            None
        } else {
            Some(&self.items[self.selected])
        }
    }
}

impl Default for CompletionPopup {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_context_analyze_normal() {
        let ctx = InputContext::analyze("hello world", 11);
        assert!(!ctx.is_command);
        assert_eq!(ctx.prefix, "world");
        assert!(ctx.command_name.is_none());
    }

    #[test]
    fn test_input_context_analyze_command() {
        let ctx = InputContext::analyze(":help me", 8);
        assert!(ctx.is_command);
        assert_eq!(ctx.command_name.as_deref(), Some(":help"));
        assert_eq!(ctx.command_args.as_deref(), Some("me"));
    }

    #[test]
    fn test_input_context_analyze_command_no_args() {
        let ctx = InputContext::analyze(":status", 7);
        assert!(ctx.is_command);
        assert_eq!(ctx.command_name.as_deref(), Some(":status"));
        assert!(ctx.command_args.is_none());
    }

    #[test]
    fn test_lightweight_completer_has_commands() {
        let completer = EvolutionCompleter::lightweight();
        assert!(!completer.builtin_commands.is_empty());
    }

    #[test]
    fn test_complete_command_matches() {
        let completer = EvolutionCompleter::lightweight();
        let ctx = InputContext::analyze(":st", 3);
        let items = completer.complete_command(&ctx);
        assert!(items.iter().any(|i| i.display == ":status"));
    }

    #[test]
    fn test_complete_command_partial() {
        let completer = EvolutionCompleter::lightweight();
        let ctx = InputContext::analyze(":he", 3);
        let items = completer.complete_command(&ctx);
        assert!(items.iter().any(|i| i.display == ":help"));
        assert!(!items.iter().any(|i| i.display == ":status"));
    }

    #[test]
    fn test_complete_normal_empty_prefix() {
        let completer = EvolutionCompleter::lightweight();
        let ctx = InputContext::analyze("", 0);
        let items = completer.complete_normal(&ctx, &[]);
        assert!(items.is_empty());
    }

    #[test]
    fn test_complete_normal_history() {
        let completer = EvolutionCompleter::lightweight();
        let ctx = InputContext::analyze("hel", 3);
        let history = vec!["hello".into(), "world".into()];
        let items = completer.complete_normal(&ctx, &history);
        assert!(items.iter().any(|i| i.display == "hello"));
    }

    #[test]
    fn test_completion_popup_navigation() {
        let mut popup = CompletionPopup::new();
        popup.set_items(vec![
            CompletionItem {
                display: "a".into(),
                replacement: "a".into(),
                category: CompletionCategory::Command,
                description: None,
            },
            CompletionItem {
                display: "b".into(),
                replacement: "b".into(),
                category: CompletionCategory::Command,
                description: None,
            },
        ]);
        assert_eq!(popup.selected, 0);
        popup.select_next();
        assert_eq!(popup.selected, 1);
        popup.select_next();
        assert_eq!(popup.selected, 0); // wrap
        popup.select_prev();
        assert_eq!(popup.selected, 1); // wrap back
    }

    #[test]
    fn test_hint_known_command() {
        let completer = EvolutionCompleter::lightweight();
        let ctx = InputContext::analyze(":status", 7);
        let hint = completer.hint(&ctx);
        assert!(hint.is_some());
        assert!(hint.unwrap().contains("系统状态"));
    }

    #[test]
    fn test_hint_unknown_text() {
        let completer = EvolutionCompleter::lightweight();
        let ctx = InputContext::analyze("hello", 5);
        let hint = completer.hint(&ctx);
        assert!(hint.is_none());
    }
}
