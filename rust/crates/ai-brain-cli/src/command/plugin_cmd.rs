//! Plugin command registration.

use super::registry::{
    ArgSpec, Command, CommandGroup, CommandHandler, CommandRegistry, CommandResult, SubCommand,
};

fn handle_plugin(args: &[String]) -> CommandResult {
    let sub = match args.first() {
        Some(s) => s.as_str(),
        None => return CommandResult::err("用法: /plugin <list|install|uninstall|reload> [参数]"),
    };

    match sub {
        "list" => CommandResult::ok("[placeholder] 已安装插件列表（暂无数据）"),
        "install" => {
            let path = args.get(1);
            match path {
                Some(p) => CommandResult::ok(format!("[placeholder] 安装插件: {p}")),
                None => CommandResult::err("用法: /plugin install <path>"),
            }
        }
        "uninstall" => {
            let name = args.get(1);
            match name {
                Some(n) => CommandResult::ok(format!("[placeholder] 卸载插件: {n}")),
                None => CommandResult::err("用法: /plugin uninstall <name>"),
            }
        }
        "reload" => CommandResult::ok("[placeholder] 重新加载所有插件"),
        other => CommandResult::err(format!(
            "未知子命令: {other}。可用: list, install, uninstall, reload"
        )),
    }
}

/// Register the `plugin` command and its subcommands.
pub fn register_plugin() -> CommandRegistry {
    let mut registry = CommandRegistry::new();

    registry.register(Command {
        name: "plugin",
        description: "插件管理",
        group: CommandGroup::Plugin,
        subcommands: vec![
            SubCommand {
                name: "list",
                description: "列出已安装的插件",
                args: vec![],
            },
            SubCommand {
                name: "install",
                description: "安装插件",
                args: vec![ArgSpec {
                    name: "path",
                    required: true,
                    completer: None,
                }],
            },
            SubCommand {
                name: "uninstall",
                description: "卸载插件",
                args: vec![ArgSpec {
                    name: "name",
                    required: true,
                    completer: None,
                }],
            },
            SubCommand {
                name: "reload",
                description: "重新加载所有插件",
                args: vec![],
            },
        ],
        handler: CommandHandler::Sync(handle_plugin),
    });

    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plugin_registration() {
        let registry = register_plugin();
        let cmd = registry
            .find_command("plugin")
            .expect("plugin command should exist");
        assert_eq!(cmd.name, "plugin");
        assert_eq!(cmd.group, CommandGroup::Plugin);
        assert_eq!(cmd.subcommands.len(), 4);
        assert_eq!(cmd.subcommands[0].name, "list");
        assert_eq!(cmd.subcommands[1].name, "install");
        assert_eq!(cmd.subcommands[2].name, "uninstall");
        assert_eq!(cmd.subcommands[3].name, "reload");
    }

    #[test]
    fn test_plugin_subcommands_found() {
        let registry = register_plugin();
        assert!(registry.find_subcommand("plugin", "list").is_some());
        assert!(registry.find_subcommand("plugin", "install").is_some());
        assert!(registry.find_subcommand("plugin", "uninstall").is_some());
        assert!(registry.find_subcommand("plugin", "reload").is_some());
        assert!(registry.find_subcommand("plugin", "nonexistent").is_none());
    }

    #[test]
    fn test_plugin_handler_no_args() {
        let registry = register_plugin();
        let cmd = registry.find_command("plugin").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&[]);
        assert!(!result.success);
    }

    #[test]
    fn test_plugin_handler_list() {
        let registry = register_plugin();
        let cmd = registry.find_command("plugin").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["list".to_string()]);
        assert!(result.success);
    }

    #[test]
    fn test_plugin_handler_install_missing_arg() {
        let registry = register_plugin();
        let cmd = registry.find_command("plugin").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["install".to_string()]);
        assert!(!result.success);
    }

    #[test]
    fn test_plugin_handler_install_with_path() {
        let registry = register_plugin();
        let cmd = registry.find_command("plugin").unwrap();
        let handler = match &cmd.handler {
            CommandHandler::Sync(h) => h,
            CommandHandler::Async(h) => h,
        };
        let result = handler(&["install".to_string(), "/tmp/plugin".to_string()]);
        assert!(result.success);
    }
}
