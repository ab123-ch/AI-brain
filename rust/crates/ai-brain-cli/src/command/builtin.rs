use super::registry::*;

/// Sentinel value returned by the `/quit` command.
pub const QUIT_SENTINEL: &str = "__QUIT__";
/// Sentinel value returned by the `/clear` command.
pub const CLEAR_SENTINEL: &str = "__CLEAR__";

fn help_handler(_args: &[String]) -> CommandResult {
    let output = "\
内置命令
  /help    — 显示帮助信息
  /status  — 显示当前状态
  /quit    — 退出程序
  /clear   — 清屏

插件命令
  /plugin list      — 列出已安装插件
  /plugin install   — 安装插件
  /plugin uninstall — 卸载插件

技能命令
  /skill list — 列出可用技能

MCP 命令
  /mcp list — 列出 MCP 连接

记忆命令
  /memory recall  — 召回记忆
  /memory search  — 搜索记忆

进化命令
  /evo        — 启动进化任务
  /evo-status — 查看进化状态

配置命令
  /config get — 查看配置
  /config set — 修改配置";
    CommandResult::ok(output)
}

fn status_handler(_args: &[String]) -> CommandResult {
    CommandResult::ok("（状态信息由 TUI 层填充）")
}

fn quit_handler(_args: &[String]) -> CommandResult {
    CommandResult::ok(QUIT_SENTINEL)
}

fn clear_handler(_args: &[String]) -> CommandResult {
    CommandResult::ok(CLEAR_SENTINEL)
}

/// Register all built-in commands and return the populated registry.
pub fn register_builtin() -> CommandRegistry {
    let mut registry = CommandRegistry::new();

    registry.register(Command {
        name: "help",
        description: "显示帮助信息",
        group: CommandGroup::BuiltIn,
        subcommands: vec![],
        handler: CommandHandler::Sync(help_handler),
    });

    registry.register(Command {
        name: "status",
        description: "显示当前状态",
        group: CommandGroup::BuiltIn,
        subcommands: vec![],
        handler: CommandHandler::Sync(status_handler),
    });

    registry.register(Command {
        name: "quit",
        description: "退出程序",
        group: CommandGroup::BuiltIn,
        subcommands: vec![],
        handler: CommandHandler::Sync(quit_handler),
    });

    registry.register(Command {
        name: "clear",
        description: "清屏",
        group: CommandGroup::BuiltIn,
        subcommands: vec![],
        handler: CommandHandler::Sync(clear_handler),
    });

    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_all() {
        let registry = register_builtin();
        assert!(registry.find_command("help").is_some(), "help should be registered");
        assert!(registry.find_command("status").is_some(), "status should be registered");
        assert!(registry.find_command("quit").is_some(), "quit should be registered");
        assert!(registry.find_command("clear").is_some(), "clear should be registered");
    }

    #[test]
    fn test_help_output() {
        let registry = register_builtin();
        let cmd = registry.find_command("help").expect("help command exists");
        let result = match &cmd.handler {
            CommandHandler::Sync(handler) => handler(&[]),
            CommandHandler::Async(handler) => handler(&[]),
        };
        assert!(result.success, "help should succeed");
        assert!(
            result.output.contains("help"),
            "help output should contain 'help', got: {}",
            result.output
        );
    }

    #[test]
    fn test_status_output() {
        let registry = register_builtin();
        let cmd = registry.find_command("status").expect("status command exists");
        let result = match &cmd.handler {
            CommandHandler::Sync(handler) => handler(&[]),
            CommandHandler::Async(handler) => handler(&[]),
        };
        assert!(result.success);
        assert_eq!(result.output, "（状态信息由 TUI 层填充）");
    }

    #[test]
    fn test_quit_sentinel() {
        let registry = register_builtin();
        let cmd = registry.find_command("quit").expect("quit command exists");
        let result = match &cmd.handler {
            CommandHandler::Sync(handler) => handler(&[]),
            CommandHandler::Async(handler) => handler(&[]),
        };
        assert!(result.success);
        assert_eq!(result.output, QUIT_SENTINEL);
    }

    #[test]
    fn test_clear_sentinel() {
        let registry = register_builtin();
        let cmd = registry.find_command("clear").expect("clear command exists");
        let result = match &cmd.handler {
            CommandHandler::Sync(handler) => handler(&[]),
            CommandHandler::Async(handler) => handler(&[]),
        };
        assert!(result.success);
        assert_eq!(result.output, CLEAR_SENTINEL);
    }
}
