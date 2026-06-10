//! Plugin command registration.

use super::registry::{
    ArgSpec, Command, CommandGroup, CommandHandler, CommandRegistry, CommandResult, SubCommand,
};
use crate::config_manager::ConfigManager;

fn handle_config(args: &[String]) -> CommandResult {
    let sub = match args.first() {
        Some(s) => s.as_str(),
        None => return CommandResult::err("用法: /config <get|set|brain-params> [参数]"),
    };

    match sub {
        "get" => {
            let key = match args.get(1) {
                Some(k) => k,
                None => return CommandResult::err("用法: /config get <key>"),
            };
            let mgr = ConfigManager::new();
            match mgr.get(key) {
                Some(value) => CommandResult::ok(format!("{key} = {value}")),
                None => CommandResult::ok(format!("{key} 未设置")),
            }
        }
        "set" => {
            let key = match args.get(1) {
                Some(k) => k,
                None => return CommandResult::err("用法: /config set <key> <value>"),
            };
            let value = match args.get(2) {
                Some(v) => v,
                None => return CommandResult::err("用法: /config set <key> <value>"),
            };
            let mut mgr = ConfigManager::new();
            match mgr.set(key, value) {
                Ok(()) => CommandResult::ok(format!("已设置 {key} = {value}")),
                Err(e) => CommandResult::err(format!("设置失败: {e}")),
            }
        }
        "brain-params" => {
            let mgr = ConfigManager::new();
            let all = mgr.all();
            if all.is_empty() {
                CommandResult::ok("配置文件为空，使用 /config set <key> <value> 添加配置")
            } else {
                let mut output = String::from("=== 配置参数 ===\n");
                for (k, v) in &all {
                    output.push_str(&format!("  {k} = {v}\n"));
                }
                CommandResult::ok(output)
            }
        }
        other => CommandResult::err(format!("未知子命令: {other}。可用: get, set, brain-params")),
    }
}

/// Register the `config` command with its 3 subcommands (get, set, brain-params).
pub fn register_config() -> CommandRegistry {
    let mut registry = CommandRegistry::new();
    registry.register(Command {
        name: "config",
        description: "配置管理",
        group: CommandGroup::Config,
        subcommands: vec![
            SubCommand {
                name: "get",
                description: "获取配置项",
                args: vec![ArgSpec {
                    name: "key",
                    required: true,
                    completer: None,
                }],
            },
            SubCommand {
                name: "set",
                description: "设置配置项",
                args: vec![
                    ArgSpec {
                        name: "key",
                        required: true,
                        completer: None,
                    },
                    ArgSpec {
                        name: "value",
                        required: true,
                        completer: None,
                    },
                ],
            },
            SubCommand {
                name: "brain-params",
                description: "查看所有配置",
                args: vec![],
            },
        ],
        handler: CommandHandler::Sync(handle_config),
    });

    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_config() {
        let registry = register_config();

        // Verify top-level command registered
        let cmd = registry
            .find_command("config")
            .expect("config command should exist");
        assert_eq!(cmd.name, "config");
        assert_eq!(cmd.group, CommandGroup::Config);

        // Verify 3 subcommands
        assert_eq!(cmd.subcommands.len(), 3);

        // get subcommand
        let get = registry
            .find_subcommand("config", "get")
            .expect("get subcommand");
        assert_eq!(get.args.len(), 1);
        assert_eq!(get.args[0].name, "key");
        assert!(get.args[0].required);

        // set subcommand
        let set = registry
            .find_subcommand("config", "set")
            .expect("set subcommand");
        assert_eq!(set.args.len(), 2);
        assert_eq!(set.args[0].name, "key");
        assert!(set.args[0].required);
        assert_eq!(set.args[1].name, "value");
        assert!(set.args[1].required);

        // brain-params subcommand
        let bp = registry
            .find_subcommand("config", "brain-params")
            .expect("brain-params subcommand");
        assert!(bp.args.is_empty());
    }

    #[test]
    fn test_config_get_missing_key() {
        let result = handle_config(&["get".to_string()]);
        assert!(!result.success);
        assert_eq!(result.output, "用法: /config get <key>");
    }

    #[test]
    fn test_config_set_missing_value() {
        let result = handle_config(&["set".to_string(), "model".to_string()]);
        assert!(!result.success);
        assert_eq!(result.output, "用法: /config set <key> <value>");
    }

    #[test]
    fn test_config_set_missing_key_and_value() {
        let result = handle_config(&["set".to_string()]);
        assert!(!result.success);
        assert_eq!(result.output, "用法: /config set <key> <value>");
    }

    #[test]
    fn test_config_unknown_subcommand() {
        let result = handle_config(&["unknown".to_string()]);
        assert!(!result.success);
        assert!(result.output.contains("未知子命令"));
    }

    #[test]
    fn test_config_no_args() {
        let result = handle_config(&[]);
        assert!(!result.success);
        assert!(result.output.contains("/config"));
    }
}
