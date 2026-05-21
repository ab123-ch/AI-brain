//! Evolver command registration.

use super::registry::{
    Command, CommandGroup, CommandHandler, CommandRegistry, CommandResult, SubCommand,
};

fn handle_evolver(args: &[String]) -> CommandResult {
    // Check if first arg is a known subcommand
    let sub = args.first().map(|s| s.as_str());

    match sub {
        Some("status") => CommandResult::ok("[placeholder] 进化任务状态（暂无进行中的任务）"),
        Some("approve") => CommandResult::ok("[placeholder] 批准进化变更"),
        Some("reject") => CommandResult::ok("[placeholder] 拒绝进化变更"),
        Some("diff") => CommandResult::ok("[placeholder] 进化变更差异（暂无数据）"),
        Some(goal) => {
            // Treat as "start evolution with goal"
            CommandResult::ok(format!("[placeholder] 启动进化任务，目标: {goal}"))
        }
        None => CommandResult::err("用法: /evo <goal> 或 /evo <status|approve|reject|diff>"),
    }
}

/// Register the `evo` command and its subcommands.
pub fn register_evolver() -> CommandRegistry {
    let mut registry = CommandRegistry::new();

    registry.register(Command {
        name: "evo",
        description: "进化脑管理",
        group: CommandGroup::Evolver,
        subcommands: vec![
            SubCommand {
                name: "status",
                description: "查看进化任务状态",
                args: vec![],
            },
            SubCommand {
                name: "approve",
                description: "批准进化变更",
                args: vec![],
            },
            SubCommand {
                name: "reject",
                description: "拒绝进化变更",
                args: vec![],
            },
            SubCommand {
                name: "diff",
                description: "查看进化变更差异",
                args: vec![],
            },
        ],
        handler: CommandHandler::Sync(handle_evolver),
    });

    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evolver_registration() {
        let registry = register_evolver();
        let cmd = registry
            .find_command("evo")
            .expect("evo command should exist");
        assert_eq!(cmd.name, "evo");
        assert_eq!(cmd.group, CommandGroup::Evolver);
        assert_eq!(cmd.subcommands.len(), 4);
        assert_eq!(cmd.subcommands[0].name, "status");
        assert_eq!(cmd.subcommands[1].name, "approve");
        assert_eq!(cmd.subcommands[2].name, "reject");
        assert_eq!(cmd.subcommands[3].name, "diff");
    }

    #[test]
    fn test_evolver_subcommands_found() {
        let registry = register_evolver();
        assert!(registry.find_subcommand("evo", "status").is_some());
        assert!(registry.find_subcommand("evo", "approve").is_some());
        assert!(registry.find_subcommand("evo", "reject").is_some());
        assert!(registry.find_subcommand("evo", "diff").is_some());
        assert!(registry.find_subcommand("evo", "nonexistent").is_none());
    }

    #[test]
    fn test_evolver_handler_no_args() {
        let registry = register_evolver();
        let cmd = registry.find_command("evo").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&[]);
        assert!(!result.success);
    }

    #[test]
    fn test_evolver_handler_subcommands() {
        let registry = register_evolver();
        let cmd = registry.find_command("evo").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        for sub in &["status", "approve", "reject", "diff"] {
            let result = handler(&[sub.to_string()]);
            assert!(result.success, "subcommand '{sub}' should succeed");
        }
    }

    #[test]
    fn test_evolver_handler_goal_arg() {
        let registry = register_evolver();
        let cmd = registry.find_command("evo").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["优化性能".to_string()]);
        assert!(result.success);
        assert!(result.output.contains("优化性能"));
    }
}
