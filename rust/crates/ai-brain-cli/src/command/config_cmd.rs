use super::*;

/// Register the "config" command with its 3 subcommands (get, set, brain-params).
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
                description: "查看脑参数",
                args: vec![],
            },
        ],
        handler: CommandHandler::Sync(handle_config),
    });
    registry
}

/// Handler for the "config" command.
/// Routes by the first arg to the appropriate subcommand handler.
fn handle_config(args: &[String]) -> CommandResult {
    match args.get(0).map(|s| s.as_str()) {
        Some("get") => handle_get(&args[1..]),
        Some("set") => handle_set(&args[1..]),
        Some("brain-params") => handle_brain_params(&args[1..]),
        _ => CommandResult::err("用法: :config get <key> | set <key> <value> | brain-params"),
    }
}

fn handle_get(args: &[String]) -> CommandResult {
    if args.is_empty() {
        return CommandResult::err("用法: :config get <key>");
    }
    let key = &args[0];
    CommandResult::ok(format!("[config] get {} = (placeholder)", key))
}

fn handle_set(args: &[String]) -> CommandResult {
    if args.len() < 2 {
        return CommandResult::err("用法: :config set <key> <value>");
    }
    let key = &args[0];
    let value = &args[1];
    CommandResult::ok(format!("[config] set {} = {} (placeholder)", key, value))
}

fn handle_brain_params(_args: &[String]) -> CommandResult {
    CommandResult::ok("[config] brain-params: (placeholder)")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_config() {
        let registry = register_config();

        // Verify top-level command registered
        let cmd = registry.find_command("config").expect("config command should exist");
        assert_eq!(cmd.name, "config");
        assert_eq!(cmd.group, CommandGroup::Config);

        // Verify 3 subcommands
        assert_eq!(cmd.subcommands.len(), 3);

        // get subcommand
        let get = registry.find_subcommand("config", "get").expect("get subcommand");
        assert_eq!(get.args.len(), 1);
        assert_eq!(get.args[0].name, "key");
        assert!(get.args[0].required);

        // set subcommand
        let set = registry.find_subcommand("config", "set").expect("set subcommand");
        assert_eq!(set.args.len(), 2);
        assert_eq!(set.args[0].name, "key");
        assert!(set.args[0].required);
        assert_eq!(set.args[1].name, "value");
        assert!(set.args[1].required);

        // brain-params subcommand
        let bp = registry.find_subcommand("config", "brain-params").expect("brain-params subcommand");
        assert!(bp.args.is_empty());
    }

    #[test]
    fn test_config_get_ok() {
        let result = handle_config(&["get".into(), "model".into()]);
        assert!(result.success);
        assert!(result.output.contains("model"));
    }

    #[test]
    fn test_config_get_missing_key() {
        let result = handle_config(&["get".into()]);
        assert!(!result.success);
        assert_eq!(result.output, "用法: :config get <key>");
    }

    #[test]
    fn test_config_set_ok() {
        let result = handle_config(&["set".into(), "model".into(), "gpt-4".into()]);
        assert!(result.success);
        assert!(result.output.contains("model"));
        assert!(result.output.contains("gpt-4"));
    }

    #[test]
    fn test_config_set_missing_value() {
        let result = handle_config(&["set".into(), "model".into()]);
        assert!(!result.success);
        assert_eq!(result.output, "用法: :config set <key> <value>");
    }

    #[test]
    fn test_config_set_missing_key_and_value() {
        let result = handle_config(&["set".into()]);
        assert!(!result.success);
        assert_eq!(result.output, "用法: :config set <key> <value>");
    }

    #[test]
    fn test_config_brain_params() {
        let result = handle_config(&["brain-params".into()]);
        assert!(result.success);
        assert!(result.output.contains("brain-params"));
    }

    #[test]
    fn test_config_unknown_subcommand() {
        let result = handle_config(&["unknown".into()]);
        assert!(!result.success);
        assert_eq!(result.output, "用法: :config get <key> | set <key> <value> | brain-params");
    }

    #[test]
    fn test_config_no_args() {
        let result = handle_config(&[]);
        assert!(!result.success);
        assert_eq!(result.output, "用法: :config get <key> | set <key> <value> | brain-params");
    }
}
