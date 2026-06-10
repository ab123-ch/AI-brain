//! Skill command registration.

use super::registry::{
    ArgSpec, Command, CommandGroup, CommandHandler, CommandRegistry, CommandResult, SubCommand,
};

fn handle_skill(args: &[String]) -> CommandResult {
    let sub = match args.first() {
        Some(s) => s.as_str(),
        None => return CommandResult::err("用法: /skill <list|run|info> [参数]"),
    };

    match sub {
        "list" => CommandResult::ok("[skill list 由 TUI app.rs 特殊处理，此处不应到达]"),
        "run" => {
            let name = args.get(1);
            match name {
                Some(n) => CommandResult::ok(format!("[placeholder] 运行技能: {n}")),
                None => CommandResult::err("用法: /skill run <name>"),
            }
        }
        "info" => {
            let name = args.get(1);
            match name {
                Some(n) => CommandResult::ok(format!(
                    "[skill info 由 TUI app.rs 特殊处理，此处不应到达: {n}]"
                )),
                None => CommandResult::err("用法: /skill info <name>"),
            }
        }
        other => CommandResult::err(format!("未知子命令: {other}。可用: list, run, info")),
    }
}

/// Register the `skill` command and its subcommands.
pub fn register_skill() -> CommandRegistry {
    let mut registry = CommandRegistry::new();

    registry.register(Command {
        name: "skill",
        description: "技能管理",
        group: CommandGroup::Skill,
        subcommands: vec![
            SubCommand {
                name: "list",
                description: "列出已注册的技能",
                args: vec![],
            },
            SubCommand {
                name: "run",
                description: "运行指定技能",
                args: vec![ArgSpec {
                    name: "name",
                    required: true,
                    completer: None,
                }],
            },
            SubCommand {
                name: "info",
                description: "查看技能详情",
                args: vec![ArgSpec {
                    name: "name",
                    required: true,
                    completer: None,
                }],
            },
        ],
        handler: CommandHandler::Sync(handle_skill),
    });

    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skill_registration() {
        let registry = register_skill();
        let cmd = registry
            .find_command("skill")
            .expect("skill command should exist");
        assert_eq!(cmd.name, "skill");
        assert_eq!(cmd.group, CommandGroup::Skill);
        assert_eq!(cmd.subcommands.len(), 3);
        assert_eq!(cmd.subcommands[0].name, "list");
        assert_eq!(cmd.subcommands[1].name, "run");
        assert_eq!(cmd.subcommands[2].name, "info");
    }

    #[test]
    fn test_skill_subcommands_found() {
        let registry = register_skill();
        assert!(registry.find_subcommand("skill", "list").is_some());
        assert!(registry.find_subcommand("skill", "run").is_some());
        assert!(registry.find_subcommand("skill", "info").is_some());
        assert!(registry.find_subcommand("skill", "nonexistent").is_none());
    }

    #[test]
    fn test_skill_handler_no_args() {
        let registry = register_skill();
        let cmd = registry.find_command("skill").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&[]);
        assert!(!result.success);
    }

    #[test]
    fn test_skill_handler_run_missing_name() {
        let registry = register_skill();
        let cmd = registry.find_command("skill").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["run".to_string()]);
        assert!(!result.success);
    }

    #[test]
    fn test_skill_handler_run_with_name() {
        let registry = register_skill();
        let cmd = registry.find_command("skill").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["run".to_string(), "my-skill".to_string()]);
        // run 仍为 placeholder，在 TUI 中由 app.rs 处理
        assert!(result.success);
    }
}
