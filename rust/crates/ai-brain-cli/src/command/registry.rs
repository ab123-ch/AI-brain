/// Command group classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandGroup {
    BuiltIn,
    Plugin,
    Skill,
    Mcp,
    Memory,
    Evolver,
    Config,
}

impl CommandGroup {
    /// Human-readable label for display purposes.
    pub fn label(&self) -> &'static str {
        match self {
            CommandGroup::BuiltIn => "内置",
            CommandGroup::Plugin => "插件",
            CommandGroup::Skill => "技能",
            CommandGroup::Mcp => "MCP",
            CommandGroup::Memory => "记忆",
            CommandGroup::Evolver => "进化",
            CommandGroup::Config => "配置",
        }
    }
}

/// Result of executing a command.
#[derive(Debug, Clone)]
pub struct CommandResult {
    pub output: String,
    pub success: bool,
}

impl CommandResult {
    /// Create a successful result with the given output text.
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            success: true,
        }
    }

    /// Create a failure result with the given error message.
    pub fn err(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            success: false,
        }
    }
}

/// Synchronous command handler function signature.
pub type SyncHandler = fn(&[String]) -> CommandResult;

/// Asynchronous command handler function signature.
pub type AsyncHandler = fn(&[String]) -> CommandResult;

/// Command handler, either synchronous or asynchronous.
#[derive(Clone)]
pub enum CommandHandler {
    Sync(SyncHandler),
    Async(AsyncHandler),
}

/// Argument specification for a subcommand.
#[derive(Clone)]
pub struct ArgSpec {
    pub name: &'static str,
    pub required: bool,
    pub completer: Option<fn() -> Vec<String>>,
}

/// A subcommand of a top-level command.
#[derive(Clone)]
pub struct SubCommand {
    pub name: &'static str,
    pub description: &'static str,
    pub args: Vec<ArgSpec>,
}

/// A top-level command definition.
#[derive(Clone)]
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
    pub group: CommandGroup,
    pub subcommands: Vec<SubCommand>,
    pub handler: CommandHandler,
}

/// The central registry for all commands.
pub struct CommandRegistry {
    commands: Vec<Command>,
}

impl CommandRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    /// Register a command.
    pub fn register(&mut self, command: Command) {
        self.commands.push(command);
    }

    /// Find a top-level command by exact name.
    pub fn find_command(&self, name: &str) -> Option<&Command> {
        self.commands.iter().find(|c| c.name == name)
    }

    /// Find a subcommand under a given top-level command.
    /// Returns `None` if the top-level command or the subcommand is not found.
    pub fn find_subcommand(&self, command_name: &str, subcommand_name: &str) -> Option<&SubCommand> {
        self.find_command(command_name)?
            .subcommands
            .iter()
            .find(|sc| sc.name == subcommand_name)
    }

    /// Filter top-level commands whose name starts with the given prefix.
    pub fn filter_commands(&self, prefix: &str) -> Vec<&Command> {
        self.commands
            .iter()
            .filter(|c| c.name.starts_with(prefix))
            .collect()
    }

    /// Merge another registry into this one, consuming the other registry.
    pub fn merge(&mut self, other: CommandRegistry) {
        self.commands.extend(other.commands);
    }

    /// Return a slice of all registered commands.
    pub fn all_commands(&self) -> &[Command] {
        &self.commands
    }

    /// Filter subcommands under a given top-level command by name prefix.
    /// Returns an empty vector if the top-level command is not found.
    pub fn filter_subcommands(&self, command_name: &str, prefix: &str) -> Vec<&SubCommand> {
        match self.find_command(command_name) {
            Some(cmd) => cmd
                .subcommands
                .iter()
                .filter(|sc| sc.name.starts_with(prefix))
                .collect(),
            None => Vec::new(),
        }
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_handler(_args: &[String]) -> CommandResult {
        CommandResult::ok("ok")
    }

    #[test]
    fn test_registry_empty() {
        let registry = CommandRegistry::new();
        assert!(registry.commands.is_empty());
        assert!(registry.find_command("help").is_none());
    }

    #[test]
    fn test_register_and_find() {
        let mut registry = CommandRegistry::new();
        registry.register(Command {
            name: "help",
            description: "Show help",
            group: CommandGroup::BuiltIn,
            subcommands: vec![],
            handler: CommandHandler::Sync(noop_handler),
        });

        let found = registry.find_command("help").expect("should find 'help'");
        assert_eq!(found.name, "help");
        assert_eq!(found.description, "Show help");
        assert_eq!(found.group, CommandGroup::BuiltIn);
        assert!(found.subcommands.is_empty());

        assert!(registry.find_command("status").is_none());
    }

    #[test]
    fn test_filter_by_prefix() {
        let mut registry = CommandRegistry::new();
        registry.register(Command {
            name: "help",
            description: "Show help",
            group: CommandGroup::BuiltIn,
            subcommands: vec![],
            handler: CommandHandler::Sync(noop_handler),
        });
        registry.register(Command {
            name: "status",
            description: "Show status",
            group: CommandGroup::BuiltIn,
            subcommands: vec![],
            handler: CommandHandler::Sync(noop_handler),
        });

        let matches = registry.filter_commands("he");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "help");

        let all = registry.filter_commands("");
        assert_eq!(all.len(), 2);

        let none = registry.filter_commands("zzz");
        assert!(none.is_empty());
    }

    #[test]
    fn test_find_subcommand() {
        let mut registry = CommandRegistry::new();
        registry.register(Command {
            name: "plugin",
            description: "Plugin management",
            group: CommandGroup::Plugin,
            subcommands: vec![
                SubCommand {
                    name: "list",
                    description: "List plugins",
                    args: vec![],
                },
                SubCommand {
                    name: "install",
                    description: "Install a plugin",
                    args: vec![ArgSpec {
                        name: "name",
                        required: true,
                        completer: None,
                    }],
                },
            ],
            handler: CommandHandler::Sync(noop_handler),
        });

        let list = registry
            .find_subcommand("plugin", "list")
            .expect("should find 'plugin list'");
        assert_eq!(list.name, "list");
        assert!(list.args.is_empty());

        let install = registry
            .find_subcommand("plugin", "install")
            .expect("should find 'plugin install'");
        assert_eq!(install.args.len(), 1);
        assert!(install.args[0].required);

        assert!(registry.find_subcommand("plugin", "remove").is_none());
        assert!(registry.find_subcommand("nonexistent", "list").is_none());
    }

    #[test]
    fn test_merge_registries() {
        let mut r1 = CommandRegistry::new();
        r1.register(Command {
            name: "help",
            description: "h",
            group: CommandGroup::BuiltIn,
            subcommands: vec![],
            handler: CommandHandler::Sync(noop_handler),
        });
        let mut r2 = CommandRegistry::new();
        r2.register(Command {
            name: "plugin",
            description: "p",
            group: CommandGroup::Plugin,
            subcommands: vec![],
            handler: CommandHandler::Sync(noop_handler),
        });
        r1.merge(r2);
        assert_eq!(r1.commands.len(), 2);
        assert_eq!(r1.find_command("help").unwrap().name, "help");
        assert_eq!(r1.find_command("plugin").unwrap().name, "plugin");
    }

    #[test]
    fn test_all_commands() {
        let mut reg = CommandRegistry::new();
        assert!(reg.all_commands().is_empty());
        reg.register(Command {
            name: "a",
            description: "a",
            group: CommandGroup::BuiltIn,
            subcommands: vec![],
            handler: CommandHandler::Sync(noop_handler),
        });
        assert_eq!(reg.all_commands().len(), 1);
        assert_eq!(reg.all_commands()[0].name, "a");
    }
}
