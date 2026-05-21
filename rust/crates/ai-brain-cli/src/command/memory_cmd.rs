//! Memory command registration.

use super::registry::{
    ArgSpec, Command, CommandGroup, CommandHandler, CommandRegistry, CommandResult, SubCommand,
};

fn handle_memory(args: &[String]) -> CommandResult {
    let sub = match args.first() {
        Some(s) => s.as_str(),
        None => return CommandResult::err("用法: /memory <stats|recall|save|daily> [参数]"),
    };

    match sub {
        "stats" => CommandResult::ok("[placeholder] 记忆统计（暂无数据）"),
        "recall" => {
            let query = args.get(1);
            match query {
                Some(q) => CommandResult::ok(format!("[placeholder] 召回记忆: {q}")),
                None => CommandResult::err("用法: /memory recall <query>"),
            }
        }
        "save" => {
            let text = args.get(1);
            match text {
                Some(t) => CommandResult::ok(format!("[placeholder] 保存记忆: {t}")),
                None => CommandResult::err("用法: /memory save <text>"),
            }
        }
        "daily" => {
            let date = args.get(1).map(|s| s.as_str()).unwrap_or("今天");
            CommandResult::ok(format!("[placeholder] 每日摘要: {date}"))
        }
        other => CommandResult::err(format!(
            "未知子命令: {other}。可用: stats, recall, save, daily"
        )),
    }
}

/// Register the `memory` command and its subcommands.
pub fn register_memory() -> CommandRegistry {
    let mut registry = CommandRegistry::new();

    registry.register(Command {
        name: "memory",
        description: "记忆管理",
        group: CommandGroup::Memory,
        subcommands: vec![
            SubCommand {
                name: "stats",
                description: "查看记忆统计",
                args: vec![],
            },
            SubCommand {
                name: "recall",
                description: "召回记忆",
                args: vec![ArgSpec {
                    name: "query",
                    required: true,
                    completer: None,
                }],
            },
            SubCommand {
                name: "save",
                description: "保存记忆",
                args: vec![ArgSpec {
                    name: "text",
                    required: true,
                    completer: None,
                }],
            },
            SubCommand {
                name: "daily",
                description: "查看每日摘要",
                args: vec![ArgSpec {
                    name: "date",
                    required: false,
                    completer: None,
                }],
            },
        ],
        handler: CommandHandler::Sync(handle_memory),
    });

    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_registration() {
        let registry = register_memory();
        let cmd = registry
            .find_command("memory")
            .expect("memory command should exist");
        assert_eq!(cmd.name, "memory");
        assert_eq!(cmd.group, CommandGroup::Memory);
        assert_eq!(cmd.subcommands.len(), 4);
        assert_eq!(cmd.subcommands[0].name, "stats");
        assert_eq!(cmd.subcommands[1].name, "recall");
        assert_eq!(cmd.subcommands[2].name, "save");
        assert_eq!(cmd.subcommands[3].name, "daily");
    }

    #[test]
    fn test_memory_daily_arg_optional() {
        let registry = register_memory();
        let daily = registry
            .find_subcommand("memory", "daily")
            .expect("daily subcommand should exist");
        assert_eq!(daily.args.len(), 1);
        assert!(!daily.args[0].required);
    }

    #[test]
    fn test_memory_handler_no_args() {
        let registry = register_memory();
        let cmd = registry.find_command("memory").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&[]);
        assert!(!result.success);
    }

    #[test]
    fn test_memory_handler_recall_no_query() {
        let registry = register_memory();
        let cmd = registry.find_command("memory").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["recall".to_string()]);
        assert!(!result.success);
    }

    #[test]
    fn test_memory_handler_daily_no_date() {
        let registry = register_memory();
        let cmd = registry.find_command("memory").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["daily".to_string()]);
        assert!(result.success);
    }

    #[test]
    fn test_memory_handler_daily_with_date() {
        let registry = register_memory();
        let cmd = registry.find_command("memory").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["daily".to_string(), "2026-05-21".to_string()]);
        assert!(result.success);
    }
}
