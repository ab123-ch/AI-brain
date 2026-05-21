use crate::command::{CommandGroup, CommandRegistry};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem};
use ratatui::Frame;

/// Maximum number of visible items in the dropdown.
const MAX_VISIBLE: usize = 6;

/// A single candidate item shown in the command panel.
#[derive(Debug, Clone)]
pub struct PanelItem {
    pub command_name: String,
    pub subcommand_name: Option<String>,
    pub description: String,
    pub display: String,
    pub group: CommandGroup,
}

/// The current phase of the command panel interaction.
#[derive(Debug, Clone, PartialEq)]
pub enum PanelPhase {
    /// Selecting a top-level command.
    Command,
    /// Selecting a subcommand under the given top-level command.
    SubCommand { command_name: String },
    /// Arguments are being typed; the panel should be hidden.
    Args {
        command_name: String,
        subcommand_name: String,
    },
}

/// State for the command panel dropdown.
pub struct CommandPanel {
    pub visible: bool,
    pub items: Vec<PanelItem>,
    pub selected: usize,
    pub phase: PanelPhase,
}

impl CommandPanel {
    /// Create a new invisible, empty panel.
    pub fn new() -> Self {
        Self {
            visible: false,
            items: Vec::new(),
            selected: 0,
            phase: PanelPhase::Command,
        }
    }

    /// Update the filtered items based on the current input text and phase.
    ///
    /// `input` is the raw text from the input area, typically starting with `:`.
    pub fn update_filter(&mut self, registry: &CommandRegistry, input: &str) {
        // Strip leading ':'
        let text = input.strip_prefix(':').unwrap_or(input);

        match &self.phase {
            PanelPhase::Command => self.filter_command_phase(registry, text),
            PanelPhase::SubCommand { command_name } => {
                let cmd_name = command_name.clone();
                // The input contains the full text (e.g. "plugin ins").
                // Strip the command name prefix + space to get the subcommand portion.
                let prefix = format!("{} ", cmd_name);
                let sub_text = if text.starts_with(&prefix) {
                    &text[prefix.len()..]
                } else if text == cmd_name {
                    ""
                } else {
                    text
                };
                self.filter_subcommand_phase(registry, &cmd_name, sub_text)
            }
            PanelPhase::Args { .. } => {
                // In args phase, hide the panel.
                self.visible = false;
            }
        }
    }

    /// Move selection down by one, wrapping around.
    pub fn move_down(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.items.len();
    }

    /// Move selection up by one, wrapping around.
    pub fn move_up(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.items.len() - 1
        } else {
            self.selected - 1
        };
    }

    /// Reset to the default invisible state.
    pub fn clear(&mut self) {
        self.visible = false;
        self.items.clear();
        self.selected = 0;
        self.phase = PanelPhase::Command;
    }

    /// Get the currently selected item, if any.
    pub fn confirm(&self) -> Option<&PanelItem> {
        if self.items.is_empty() || !self.visible {
            return None;
        }
        self.items.get(self.selected)
    }

    /// Render the dropdown panel above the given input area.
    pub fn render(&self, frame: &mut Frame, input_area: Rect) {
        if !self.visible || self.items.is_empty() {
            return;
        }

        let visible_count = self.items.len().min(MAX_VISIBLE);
        let panel_height = visible_count as u16 + 2; // +2 for borders

        // Position above the input area
        let panel_y = input_area.y.saturating_sub(panel_height);
        let panel_rect = Rect {
            x: input_area.x,
            y: panel_y,
            width: input_area.width,
            height: panel_height,
        };

        // Clear the area behind the panel
        frame.render_widget(Clear, panel_rect);

        let items: Vec<ListItem> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let is_selected = i == self.selected;
                let style = if is_selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };

                let command_span = Span::styled(format!(":{}", item.command_name), style);
                let sep = Span::styled(" \u{2014} ", style);
                let group_span = Span::styled(item.group.label(), style);
                let desc_span = Span::styled(format!(" {}", item.description), style);

                let subcommand_span = if let Some(ref sc) = item.subcommand_name {
                    Span::styled(format!(" {}", sc), style)
                } else {
                    Span::styled(String::new(), style)
                };

                let line = Line::from(vec![
                    command_span,
                    subcommand_span,
                    sep,
                    group_span,
                    desc_span,
                ]);

                ListItem::new(line).style(if is_selected {
                    Style::default().bg(Color::Cyan)
                } else {
                    Style::default().bg(Color::DarkGray)
                })
            })
            .collect();

        let list = List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .style(Style::default().bg(Color::DarkGray)),
        );

        frame.render_widget(list, panel_rect);
    }

    // -- Private helpers --

    fn filter_command_phase(&mut self, registry: &CommandRegistry, text: &str) {
        // Split on first space to detect subcommand transition
        if let Some(space_pos) = text.find(' ') {
            let cmd_part = &text[..space_pos];
            let subcmd_prefix = text[space_pos + 1..].trim_start();

            // Check if the command exists
            if registry.find_command(cmd_part).is_some() {
                // Switch to SubCommand phase
                self.phase = PanelPhase::SubCommand {
                    command_name: cmd_part.to_string(),
                };
                self.filter_subcommand_phase(registry, cmd_part, subcmd_prefix);
                return;
            }
            // Command not found with the prefix before space, fall through to filter
        }

        let prefix = text.trim();
        let commands = registry.filter_commands(prefix);

        if commands.is_empty() {
            self.visible = false;
            self.items.clear();
            self.selected = 0;
            return;
        }

        self.visible = true;
        self.items = commands
            .into_iter()
            .map(|cmd| PanelItem {
                command_name: cmd.name.to_string(),
                subcommand_name: None,
                description: cmd.description.to_string(),
                display: format!(":{}", cmd.name),
                group: cmd.group,
            })
            .collect();
        self.selected = 0;
    }

    fn filter_subcommand_phase(
        &mut self,
        registry: &CommandRegistry,
        command_name: &str,
        text: &str,
    ) {
        // If a second space is found, switch to Args phase
        if let Some(space_pos) = text.find(' ') {
            let subcmd_part = &text[..space_pos];

            // Check if the subcommand exists
            if registry
                .find_subcommand(command_name, subcmd_part)
                .is_some()
            {
                self.phase = PanelPhase::Args {
                    command_name: command_name.to_string(),
                    subcommand_name: subcmd_part.to_string(),
                };
                self.visible = false;
                self.items.clear();
                return;
            }
        }

        let prefix = text.trim();
        let subcommands = registry.filter_subcommands(command_name, prefix);

        if subcommands.is_empty() {
            self.visible = false;
            self.items.clear();
            self.selected = 0;
            return;
        }

        self.visible = true;
        self.items = subcommands
            .into_iter()
            .map(|sc| PanelItem {
                command_name: command_name.to_string(),
                subcommand_name: Some(sc.name.to_string()),
                description: sc.description.to_string(),
                display: format!(":{} {}", command_name, sc.name),
                group: registry
                    .find_command(command_name)
                    .map(|c| c.group)
                    .unwrap_or(CommandGroup::BuiltIn),
            })
            .collect();
        self.selected = 0;
    }
}

impl Default for CommandPanel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Command, CommandHandler, CommandResult, SubCommand};

    fn noop_handler(_args: &[String]) -> CommandResult {
        CommandResult::ok("ok")
    }

    /// Build a minimal registry for testing.
    fn test_registry() -> CommandRegistry {
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
                    args: vec![],
                },
                SubCommand {
                    name: "uninstall",
                    description: "Uninstall a plugin",
                    args: vec![],
                },
            ],
            handler: CommandHandler::Sync(noop_handler),
        });
        registry
    }

    #[test]
    fn test_panel_initial_state() {
        let panel = CommandPanel::new();
        assert!(!panel.visible);
        assert!(panel.items.is_empty());
        assert_eq!(panel.selected, 0);
        assert_eq!(panel.phase, PanelPhase::Command);
    }

    #[test]
    fn test_filter_commands() {
        let registry = test_registry();
        let mut panel = CommandPanel::new();

        panel.update_filter(&registry, "he");

        assert!(panel.visible);
        assert_eq!(panel.items.len(), 1);
        assert_eq!(panel.items[0].command_name, "help");
        assert_eq!(panel.items[0].display, ":help");
        assert!(panel.items[0].subcommand_name.is_none());
    }

    #[test]
    fn test_filter_shows_all_on_empty() {
        let registry = test_registry();
        let mut panel = CommandPanel::new();

        panel.update_filter(&registry, "");

        assert!(panel.visible);
        // help, status, plugin
        assert_eq!(panel.items.len(), 3);
    }

    #[test]
    fn test_navigation() {
        let registry = test_registry();
        let mut panel = CommandPanel::new();
        panel.update_filter(&registry, "");

        // 3 items: help(0), status(1), plugin(2)
        assert_eq!(panel.selected, 0);

        panel.move_down();
        assert_eq!(panel.selected, 1);

        panel.move_down();
        assert_eq!(panel.selected, 2);

        // Wrap around
        panel.move_down();
        assert_eq!(panel.selected, 0);

        // Move up from 0 should wrap to last
        panel.move_up();
        assert_eq!(panel.selected, 2);

        panel.move_up();
        assert_eq!(panel.selected, 1);
    }

    #[test]
    fn test_navigation_empty() {
        let mut panel = CommandPanel::new();
        // Should not panic on empty items
        panel.move_down();
        panel.move_up();
        assert_eq!(panel.selected, 0);
    }

    #[test]
    fn test_clear() {
        let registry = test_registry();
        let mut panel = CommandPanel::new();
        panel.update_filter(&registry, "he");
        assert!(panel.visible);

        panel.clear();
        assert!(!panel.visible);
        assert!(panel.items.is_empty());
        assert_eq!(panel.selected, 0);
        assert_eq!(panel.phase, PanelPhase::Command);
    }

    #[test]
    fn test_confirm() {
        let registry = test_registry();
        let mut panel = CommandPanel::new();
        panel.update_filter(&registry, "he");

        let item = panel.confirm().expect("should have a selection");
        assert_eq!(item.command_name, "help");
    }

    #[test]
    fn test_confirm_invisible() {
        let panel = CommandPanel::new();
        assert!(panel.confirm().is_none());
    }

    #[test]
    fn test_panel_subcommand_mode() {
        let registry = test_registry();
        let mut panel = CommandPanel::new();

        // Typing "plugin " (with trailing space) should show subcommands
        panel.update_filter(&registry, "plugin ");

        assert!(panel.visible);
        assert_eq!(
            panel.phase,
            PanelPhase::SubCommand {
                command_name: "plugin".to_string()
            }
        );
        // list, install, uninstall
        assert_eq!(panel.items.len(), 3);
        assert_eq!(panel.items[0].subcommand_name.as_deref(), Some("list"));
        assert_eq!(panel.items[1].subcommand_name.as_deref(), Some("install"));
        assert_eq!(panel.items[2].subcommand_name.as_deref(), Some("uninstall"));
    }

    #[test]
    fn test_panel_subcommand_filter() {
        let registry = test_registry();
        let mut panel = CommandPanel::new();

        // Typing "plugin ins" should filter subcommands by prefix "ins"
        panel.update_filter(&registry, "plugin ins");

        assert!(panel.visible);
        assert_eq!(panel.items.len(), 1);
        assert_eq!(panel.items[0].subcommand_name.as_deref(), Some("install"));
    }

    #[test]
    fn test_panel_no_match() {
        let registry = test_registry();
        let mut panel = CommandPanel::new();

        panel.update_filter(&registry, "zzz");

        assert!(!panel.visible);
        assert!(panel.items.is_empty());
    }

    #[test]
    fn test_panel_args_phase() {
        let registry = test_registry();
        let mut panel = CommandPanel::new();

        // First go to subcommand phase
        panel.update_filter(&registry, "plugin ");
        assert!(panel.visible);

        // Now type "install something" — should transition to Args phase
        panel.update_filter(&registry, "plugin install something");

        assert!(!panel.visible);
        assert_eq!(
            panel.phase,
            PanelPhase::Args {
                command_name: "plugin".to_string(),
                subcommand_name: "install".to_string()
            }
        );
    }

    #[test]
    fn test_panel_without_colon_prefix() {
        let registry = test_registry();
        let mut panel = CommandPanel::new();

        // Should work even without leading ':'
        panel.update_filter(&registry, "he");

        assert!(panel.visible);
        assert_eq!(panel.items.len(), 1);
        assert_eq!(panel.items[0].command_name, "help");
    }

    #[test]
    fn test_panel_with_colon_prefix() {
        let registry = test_registry();
        let mut panel = CommandPanel::new();

        panel.update_filter(&registry, ":he");

        assert!(panel.visible);
        assert_eq!(panel.items.len(), 1);
        assert_eq!(panel.items[0].command_name, "help");
    }
}

/// Integration tests using the full command registry from build_full_registry().
/// These verify end-to-end interaction between the command panel and all registered commands.
#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::command;

    fn full_registry() -> CommandRegistry {
        command::build_full_registry()
    }

    #[test]
    fn test_full_registry_has_all_commands() {
        let reg = full_registry();
        // Verify all 10 commands are registered
        assert!(reg.find_command("help").is_some(), "missing help");
        assert!(reg.find_command("status").is_some(), "missing status");
        assert!(reg.find_command("quit").is_some(), "missing quit");
        assert!(reg.find_command("clear").is_some(), "missing clear");
        assert!(reg.find_command("config").is_some(), "missing config");
        assert!(reg.find_command("plugin").is_some(), "missing plugin");
        assert!(reg.find_command("skill").is_some(), "missing skill");
        assert!(reg.find_command("mcp").is_some(), "missing mcp");
        assert!(reg.find_command("memory").is_some(), "missing memory");
        assert!(reg.find_command("evo").is_some(), "missing evo");
    }

    #[test]
    fn test_panel_filter_plugin() {
        let mut panel = CommandPanel::new();
        panel.update_filter(&full_registry(), "pl");
        assert!(panel.visible);
        assert!(panel.items.iter().any(|i| i.command_name == "plugin"));
        assert!(!panel.items.iter().any(|i| i.command_name == "help"));
    }

    #[test]
    fn test_panel_subcommand_mode() {
        let mut panel = CommandPanel::new();
        panel.update_filter(&full_registry(), "plugin ");
        assert!(panel.visible);
        assert!(panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("list")));
        assert!(panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("install")));
    }

    #[test]
    fn test_panel_subcommand_filter() {
        let mut panel = CommandPanel::new();
        panel.update_filter(&full_registry(), "plugin ins");
        assert!(panel.visible);
        assert!(panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("install")));
        assert!(!panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("list")));
    }

    #[test]
    fn test_panel_no_match() {
        let mut panel = CommandPanel::new();
        panel.update_filter(&full_registry(), "zzz");
        assert!(!panel.visible);
    }

    #[test]
    fn test_panel_memory_subcommands() {
        let mut panel = CommandPanel::new();
        panel.update_filter(&full_registry(), "memory ");
        assert!(panel.visible);
        assert!(panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("stats")));
        assert!(panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("recall")));
        assert!(panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("save")));
        assert!(panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("daily")));
    }

    #[test]
    fn test_panel_evo_filter() {
        let mut panel = CommandPanel::new();
        panel.update_filter(&full_registry(), "ev");
        assert!(panel.visible);
        assert!(panel.items.iter().any(|i| i.command_name == "evo"));
    }

    #[test]
    fn test_panel_config_subcommands() {
        let mut panel = CommandPanel::new();
        panel.update_filter(&full_registry(), "config ");
        assert!(panel.visible);
        assert!(panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("get")));
        assert!(panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("set")));
        assert!(panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("brain-params")));
    }

    #[test]
    fn test_panel_skill_subcommands() {
        let mut panel = CommandPanel::new();
        panel.update_filter(&full_registry(), "skill ");
        assert!(panel.visible);
        assert!(panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("list")));
        assert!(panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("run")));
        assert!(panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("info")));
    }

    #[test]
    fn test_panel_mcp_subcommands() {
        let mut panel = CommandPanel::new();
        panel.update_filter(&full_registry(), "mcp ");
        assert!(panel.visible);
        assert!(panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("list")));
        assert!(panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("status")));
        assert!(panel
            .items
            .iter()
            .any(|i| i.subcommand_name.as_deref() == Some("reconnect")));
    }

    #[test]
    fn test_panel_navigation_and_confirm() {
        let mut panel = CommandPanel::new();
        panel.update_filter(&full_registry(), "pl");
        assert!(panel.visible);
        assert_eq!(panel.selected, 0);

        panel.move_down();
        // Only "plugin" matches "pl", so move_down wraps back to 0
        let item = panel.confirm().expect("should have a selection");
        assert_eq!(item.command_name, "plugin");
    }
}
