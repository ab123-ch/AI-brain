//! Persona command registration.
//!
//! 人格管理命令: list, switch, info, create, delete

use super::registry::{
    ArgSpec, Command, CommandGroup, CommandHandler, CommandRegistry, CommandResult, SubCommand,
};

fn handle_persona(args: &[String]) -> CommandResult {
    let sub = match args.first() {
        Some(s) => s.as_str(),
        None => {
            return CommandResult::err("用法: /persona <list|switch|info|create|delete> [参数]")
        }
    };

    match sub {
        "list" => CommandResult::ok("[persona list 由 TUI app.rs 特殊处理，此处不应到达]"),
        "switch" => {
            let id = args.get(1);
            match id {
                Some(id) => CommandResult::ok(format!(
                    "[persona switch 由 TUI app.rs 特殊处理，此处不应到达: {id}]"
                )),
                None => CommandResult::err("用法: /persona switch <id>"),
            }
        }
        "info" => CommandResult::ok("[persona info 由 TUI app.rs 特殊处理，此处不应到达]"),
        "create" => CommandResult::ok("[persona create 由 TUI app.rs 特殊处理，此处不应到达]"),
        "delete" => {
            let id = args.get(1);
            match id {
                Some(id) => CommandResult::ok(format!(
                    "[persona delete 由 TUI app.rs 特殊处理，此处不应到达: {id}]"
                )),
                None => CommandResult::err("用法: /persona delete <id>"),
            }
        }
        other => CommandResult::err(format!(
            "未知子命令: {other}。可用: list, switch, info, create, delete"
        )),
    }
}

/// Register the `persona` command and its subcommands.
pub fn register_persona() -> CommandRegistry {
    let mut registry = CommandRegistry::new();

    registry.register(Command {
        name: "persona",
        description: "人格管理",
        group: CommandGroup::BuiltIn,
        subcommands: vec![
            SubCommand {
                name: "list",
                description: "列出所有人格",
                args: vec![],
            },
            SubCommand {
                name: "switch",
                description: "切换人格",
                args: vec![ArgSpec {
                    name: "id",
                    required: true,
                    completer: None,
                }],
            },
            SubCommand {
                name: "info",
                description: "当前人格详情",
                args: vec![],
            },
            SubCommand {
                name: "create",
                description: "创建新人格",
                args: vec![],
            },
            SubCommand {
                name: "delete",
                description: "删除人格",
                args: vec![ArgSpec {
                    name: "id",
                    required: true,
                    completer: None,
                }],
            },
        ],
        handler: CommandHandler::Sync(handle_persona),
    });

    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_persona_registration() {
        let registry = register_persona();
        let cmd = registry
            .find_command("persona")
            .expect("persona command should exist");
        assert_eq!(cmd.name, "persona");
        assert_eq!(cmd.group, CommandGroup::BuiltIn);
        assert_eq!(cmd.subcommands.len(), 5);
        assert_eq!(cmd.subcommands[0].name, "list");
        assert_eq!(cmd.subcommands[1].name, "switch");
        assert_eq!(cmd.subcommands[2].name, "info");
        assert_eq!(cmd.subcommands[3].name, "create");
        assert_eq!(cmd.subcommands[4].name, "delete");
    }

    #[test]
    fn test_persona_subcommands_found() {
        let registry = register_persona();
        assert!(registry.find_subcommand("persona", "list").is_some());
        assert!(registry.find_subcommand("persona", "switch").is_some());
        assert!(registry.find_subcommand("persona", "info").is_some());
        assert!(registry.find_subcommand("persona", "create").is_some());
        assert!(registry.find_subcommand("persona", "delete").is_some());
        assert!(registry.find_subcommand("persona", "nonexistent").is_none());
    }

    #[test]
    fn test_persona_handler_no_args() {
        let registry = register_persona();
        let cmd = registry.find_command("persona").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&[]);
        assert!(!result.success);
    }

    #[test]
    fn test_persona_handler_switch_no_id() {
        let registry = register_persona();
        let cmd = registry.find_command("persona").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["switch".to_string()]);
        assert!(!result.success);
        assert!(result.output.contains("switch <id>"));
    }

    #[test]
    fn test_persona_handler_switch_with_id() {
        let registry = register_persona();
        let cmd = registry.find_command("persona").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["switch".to_string(), "writer".to_string()]);
        assert!(result.success);
    }

    #[test]
    fn test_persona_handler_delete_no_id() {
        let registry = register_persona();
        let cmd = registry.find_command("persona").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["delete".to_string()]);
        assert!(!result.success);
    }

    #[test]
    fn test_persona_handler_unknown_sub() {
        let registry = register_persona();
        let cmd = registry.find_command("persona").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["unknown".to_string()]);
        assert!(!result.success);
        assert!(result.output.contains("未知子命令"));
    }
}
