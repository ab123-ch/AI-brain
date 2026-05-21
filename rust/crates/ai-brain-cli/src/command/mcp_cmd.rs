//! MCP command registration.

use super::registry::{
    ArgSpec, Command, CommandGroup, CommandHandler, CommandRegistry, CommandResult, SubCommand,
};

fn handle_mcp(args: &[String]) -> CommandResult {
    let sub = match args.first() {
        Some(s) => s.as_str(),
        None => return CommandResult::err("用法: /mcp <list|status|reconnect> [参数]"),
    };

    match sub {
        "list" => CommandResult::ok("[placeholder] MCP 服务列表（暂无数据）"),
        "status" => {
            let server = args.get(1);
            match server {
                Some(s) => CommandResult::ok(format!("[placeholder] MCP 服务状态: {s}")),
                None => CommandResult::err("用法: /mcp status <server>"),
            }
        }
        "reconnect" => {
            let server = args.get(1);
            match server {
                Some(s) => CommandResult::ok(format!("[placeholder] 重新连接 MCP 服务: {s}")),
                None => CommandResult::err("用法: /mcp reconnect <server>"),
            }
        }
        other => CommandResult::err(format!(
            "未知子命令: {other}。可用: list, status, reconnect"
        )),
    }
}

/// Register the `mcp` command and its subcommands.
pub fn register_mcp() -> CommandRegistry {
    let mut registry = CommandRegistry::new();

    registry.register(Command {
        name: "mcp",
        description: "MCP 服务管理",
        group: CommandGroup::Mcp,
        subcommands: vec![
            SubCommand {
                name: "list",
                description: "列出 MCP 服务",
                args: vec![],
            },
            SubCommand {
                name: "status",
                description: "查看 MCP 服务状态",
                args: vec![ArgSpec {
                    name: "server",
                    required: true,
                    completer: None,
                }],
            },
            SubCommand {
                name: "reconnect",
                description: "重新连接 MCP 服务",
                args: vec![ArgSpec {
                    name: "server",
                    required: true,
                    completer: None,
                }],
            },
        ],
        handler: CommandHandler::Sync(handle_mcp),
    });

    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_registration() {
        let registry = register_mcp();
        let cmd = registry
            .find_command("mcp")
            .expect("mcp command should exist");
        assert_eq!(cmd.name, "mcp");
        assert_eq!(cmd.group, CommandGroup::Mcp);
        assert_eq!(cmd.subcommands.len(), 3);
        assert_eq!(cmd.subcommands[0].name, "list");
        assert_eq!(cmd.subcommands[1].name, "status");
        assert_eq!(cmd.subcommands[2].name, "reconnect");
    }

    #[test]
    fn test_mcp_subcommands_found() {
        let registry = register_mcp();
        assert!(registry.find_subcommand("mcp", "list").is_some());
        assert!(registry.find_subcommand("mcp", "status").is_some());
        assert!(registry.find_subcommand("mcp", "reconnect").is_some());
        assert!(registry.find_subcommand("mcp", "nonexistent").is_none());
    }

    #[test]
    fn test_mcp_handler_no_args() {
        let registry = register_mcp();
        let cmd = registry.find_command("mcp").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&[]);
        assert!(!result.success);
    }

    #[test]
    fn test_mcp_handler_status_missing_server() {
        let registry = register_mcp();
        let cmd = registry.find_command("mcp").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["status".to_string()]);
        assert!(!result.success);
    }

    #[test]
    fn test_mcp_handler_status_with_server() {
        let registry = register_mcp();
        let cmd = registry.find_command("mcp").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["status".to_string(), "my-server".to_string()]);
        assert!(result.success);
    }
}
