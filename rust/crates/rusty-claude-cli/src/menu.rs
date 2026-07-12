//! Interactive slash-command menu.
//!
//! When the user types `/` and presses Tab (or a dedicated key), this module
//! opens a searchable, categorised panel showing slash commands, installed
//! skills, plugins, and agents. The user navigates with arrow keys, filters
//! by typing, and confirms the selection with Enter. The result is then
//! inserted into the readline buffer and submitted.

use crossterm::{
    cursor::{self, MoveTo},
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    style::{self, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, Clear, ClearType},
    QueueableCommand,
};
use std::cmp::min;
use std::io::{self, Write};

/// A single entry that can appear inside the popup menu.
#[derive(Debug, Clone)]
pub struct MenuEntry {
    /// Visible label (e.g. "/help", "$find-skills", "/plugin list").
    pub label: String,
    /// One-line description.
    pub description: String,
    /// Category tag shown in the right gutter.
    pub category: MenuCategory,
    /// The literal string to insert into the input buffer when selected.
    pub insert_text: String,
}

/// Category icons / tags for grouping entries visually.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCategory {
    SlashCommand,
    Skill,
    Plugin,
    Agent,
}

impl MenuCategory {
    fn icon(self) -> &'static str {
        match self {
            Self::SlashCommand => "⚡",
            Self::Skill => "📦",
            Self::Plugin => "🔌",
            Self::Agent => "🤖",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::SlashCommand => "Slash Commands",
            Self::Skill => "Skills",
            Self::Plugin => "Plugins",
            Self::Agent => "Agents",
        }
    }
}

// ── Internal state ────────────────────────────────────────────────────────

struct MenuState {
    entries: Vec<MenuEntry>,
    filter: String,
    selection: usize,
    visible_count: usize,
    scroll_offset: usize,
}

impl MenuState {
    fn new(entries: Vec<MenuEntry>) -> Self {
        let len = entries.len();
        Self {
            entries,
            filter: String::new(),
            selection: 0,
            visible_count: len,
            scroll_offset: 0,
        }
    }

    /// Recompute which entries match the current filter.
    fn apply_filter(&mut self) {
        let lower = self.filter.to_ascii_lowercase();
        if lower.is_empty() {
            self.visible_count = self.entries.len();
            self.selection = 0;
            self.scroll_offset = 0;
            return;
        }

        // Count matching entries by scanning
        let mut matched = 0usize;
        let mut first_selected = None;
        for (i, entry) in self.entries.iter().enumerate() {
            let haystack = format!("{} {}", entry.label, entry.description).to_ascii_lowercase();
            if haystack.contains(&lower) {
                if first_selected.is_none() {
                    first_selected = Some(matched);
                }
                matched += 1;
                // Only need to track selection position, not store filtered list
            }
        }
        self.visible_count = matched;
        self.selection = first_selected.unwrap_or(0);
        self.scroll_offset = 0;
    }

    /// Map the visible-selecton index back to the real entry index.
    fn real_index(&self) -> Option<usize> {
        let lower = self.filter.to_ascii_lowercase();
        let mut vis = 0usize;
        for (i, entry) in self.entries.iter().enumerate() {
            let haystack = format!("{} {}", entry.label, entry.description).to_ascii_lowercase();
            let matches = lower.is_empty() || haystack.contains(&lower);
            if !matches {
                continue;
            }
            if vis == self.selection {
                return Some(i);
            }
            vis += 1;
        }
        None
    }

    fn selected_entry(&self) -> Option<&MenuEntry> {
        self.real_index().map(|i| &self.entries[i])
    }

    fn move_up(&mut self) {
        if self.selection > 0 {
            self.selection -= 1;
        }
    }

    fn move_down(&mut self) {
        if self.selection + 1 < self.visible_count {
            self.selection += 1;
        }
    }

    fn push_char(&mut self, c: char) {
        self.filter.push(c);
        self.apply_filter();
    }

    fn pop_char(&mut self) {
        self.filter.pop();
        self.apply_filter();
    }
}

// ── Rendering constants ───────────────────────────────────────────────────

const MENU_MIN_WIDTH: u16 = 60;
const MENU_MAX_HEIGHT: u16 = 20;
const FOOTER_HEIGHT: u16 = 2; // help line + bottom border
const HEADER_HEIGHT: u16 = 1; // filter line
const ITEM_PADDING: u16 = 1; // top/bottom padding
const BORDER_WIDTH: u16 = 2; // left + right border chars

impl MenuCategory {
    fn color(self) -> Color {
        match self {
            Self::SlashCommand => Color::Cyan,
            Self::Skill => Color::Yellow,
            Self::Plugin => Color::Magenta,
            Self::Agent => Color::Green,
        }
    }
}

// ── Public entry point ────────────────────────────────────────────────────

/// Open the interactive slash menu and return the text the user selected
/// (or `None` if they cancelled).
///
/// This function:
/// 1. Saves the terminal state
/// 2. Draws the menu panel
/// 3. Processes keyboard events until the user selects or cancels
/// 4. Restores the terminal state and returns
pub fn show_menu(stdout: &mut io::Stdout, entries: Vec<MenuEntry>) -> io::Result<Option<String>> {
    if entries.is_empty() {
        return Ok(None);
    }

    // Determine terminal size once.
    let (term_width, term_height) = terminal::size()?;
    let menu_width = MENU_MIN_WIDTH.max(min(term_width, 120));
    let body_height = MENU_MAX_HEIGHT
        .min(term_height.saturating_sub(2))
        .min((entries.len() as u16).saturating_add(ITEM_PADDING * 2));
    let menu_height = HEADER_HEIGHT + body_height + FOOTER_HEIGHT;

    // Panel left offset (centred).
    let left = (term_width.saturating_sub(menu_width)) / 2;
    // Panel top offset (centred vertically, biased upward).
    let top = (term_height.saturating_sub(menu_height)) / 2;

    let mut state = MenuState::new(entries);

    // Hide cursor while the menu is open.
    stdout.queue(cursor::Hide)?;
    let _ = draw_panel(stdout, left, top, menu_width, body_height, &state);

    let result = loop {
        match event::read()? {
            Event::Key(KeyEvent {
                code: KeyCode::Char('q'),
                modifiers: KeyModifiers::NONE,
                ..
            }) => {
                // Quit menu without selecting.
                break Ok(None);
            }
            Event::Key(KeyEvent {
                code: KeyCode::Esc, ..
            }) => {
                break Ok(None);
            }
            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                ..
            }) => {
                let text = state.selected_entry().map(|e| e.insert_text.clone());
                break Ok(text);
            }
            Event::Key(KeyEvent {
                code: KeyCode::Up, ..
            }) => {
                state.move_up();
                let _ = draw_panel(stdout, left, top, menu_width, body_height, &state);
            }
            Event::Key(KeyEvent {
                code: KeyCode::Down,
                ..
            }) => {
                state.move_down();
                let _ = draw_panel(stdout, left, top, menu_width, body_height, &state);
            }
            Event::Key(KeyEvent {
                code: KeyCode::PageUp,
                ..
            }) => {
                for _ in 0..10 {
                    state.move_up();
                }
                let _ = draw_panel(stdout, left, top, menu_width, body_height, &state);
            }
            Event::Key(KeyEvent {
                code: KeyCode::PageDown,
                ..
            }) => {
                for _ in 0..10 {
                    state.move_down();
                }
                let _ = draw_panel(stdout, left, top, menu_width, body_height, &state);
            }
            Event::Key(KeyEvent {
                code: KeyCode::Home,
                ..
            }) => {
                state.selection = 0;
                let _ = draw_panel(stdout, left, top, menu_width, body_height, &state);
            }
            Event::Key(KeyEvent {
                code: KeyCode::End, ..
            }) => {
                state.selection = state.visible_count.saturating_sub(1);
                let _ = draw_panel(stdout, left, top, menu_width, body_height, &state);
            }
            Event::Key(KeyEvent {
                code: KeyCode::Backspace,
                ..
            }) => {
                state.pop_char();
                let _ = draw_panel(stdout, left, top, menu_width, body_height, &state);
            }
            Event::Key(KeyEvent {
                code: KeyCode::Tab, ..
            }) => {
                // Cycle selection to next entry (Tab as quick-select).
                state.move_down();
                let _ = draw_panel(stdout, left, top, menu_width, body_height, &state);
            }
            Event::Key(KeyEvent {
                code: KeyCode::Char(c),
                ..
            }) => {
                state.push_char(c);
                let _ = draw_panel(stdout, left, top, menu_width, body_height, &state);
            }
            Event::Resize(cols, rows) => {
                // Recalculate layout on terminal resize.
                let menu_width = MENU_MIN_WIDTH.max(min(cols, 120));
                let body_height = MENU_MAX_HEIGHT
                    .min(rows.saturating_sub(2))
                    .min((state.entries.len() as u16).saturating_add(ITEM_PADDING * 2));
                let left = (cols.saturating_sub(menu_width)) / 2;
                let top = (rows.saturating_sub(menu_height)) / 2;
                let _ = draw_panel(stdout, left, top, menu_width, body_height, &state);
            }
            _ => {}
        }
    };

    // Clean up.
    stdout.queue(cursor::Show)?;
    stdout.flush()?;
    result
}

// ── Drawing ───────────────────────────────────────────────────────────────

fn draw_panel(
    stdout: &mut io::Stdout,
    left: u16,
    top: u16,
    menu_width: u16,
    body_height: u16,
    state: &MenuState,
) -> io::Result<()> {
    let content_width = menu_width.saturating_sub(BORDER_WIDTH);

    // ── Filter line (header) ──
    stdout.queue(MoveTo(left, top))?;
    stdout
        .queue(SetForegroundColor(Color::White))?
        .queue(SetAttribute(style::Attribute::Bold))?
        .queue(Print("┌─ "))?
        .queue(ResetColor)?
        .queue(Print("Filter"))?;
    // Draw the filter value
    let filter_display = if state.filter.is_empty() {
        " type to search…".to_string()
    } else {
        format!(" {}", state.filter)
    };
    // Right-align count badge
    let count_str = format!(" {} matches ", state.visible_count);
    let remaining =
        content_width.saturating_sub(filter_display.len() as u16 + count_str.len() as u16 + 4);
    let padding = " ".repeat(remaining.max(1) as usize);
    stdout
        .queue(SetForegroundColor(Color::DarkGrey))?
        .queue(Print(&filter_display))?
        .queue(Print(&padding))?
        .queue(ResetColor)?
        .queue(SetForegroundColor(Color::Green))?
        .queue(Print(&count_str))?
        .queue(ResetColor)?;

    // Draw top-right corner continuation
    stdout.queue(Print("─┐"))?;

    // ── Body ──
    let lower_filter = state.filter.to_ascii_lowercase();

    // Compute which visible entries to show
    let mut visible_indices: Vec<usize> = Vec::new();
    for (i, entry) in state.entries.iter().enumerate() {
        let haystack = format!("{} {}", entry.label, entry.description).to_ascii_lowercase();
        if lower_filter.is_empty() || haystack.contains(&lower_filter) {
            visible_indices.push(i);
        }
    }
    let total_visible = visible_indices.len();

    // Clamp scroll so selection stays visible
    let scroll = if state.selection >= body_height as usize {
        state
            .selection
            .saturating_sub(body_height as usize)
            .saturating_add(1)
    } else {
        0
    };
    let scroll = scroll.min(total_visible.saturating_sub(1));

    let range_start = scroll;
    let range_end = min(scroll + body_height as usize, total_visible);

    // Draw each visible line
    for line_index in 0..body_height {
        let y = top + 1 + line_index;
        stdout.queue(MoveTo(left, y))?;
        stdout.queue(Print("│ "))?;

        let entry_index = range_start + line_index as usize;
        if entry_index < total_visible {
            let real_i = visible_indices[entry_index];
            let entry = &state.entries[real_i];
            let is_selected = entry_index == state.selection.min(total_visible.saturating_sub(1));

            if is_selected {
                stdout.queue(SetForegroundColor(Color::Black))?;
                stdout.queue(style::SetBackgroundColor(Color::White))?;
            }

            // Category icon + label
            let icon = entry.category.icon();
            let label_str = &entry.label;

            // Truncate label to fit
            let max_label_len = (content_width as usize).saturating_sub(4); // icon + space + desc hint + padding
            let display_label = if label_str.len() > max_label_len {
                format!("{}…", &label_str[..max_label_len.saturating_sub(1)])
            } else {
                label_str.clone()
            };

            write!(stdout, "{} {}", icon, display_label)?;

            // Fill remaining space and show description on the right
            let cat_tag = if is_selected {
                format!(" {}", entry.category.label())
            } else {
                String::new()
            };
            let desc_preview: &str = if entry.description.len() > 30 {
                &entry.description[..27]
            } else {
                &entry.description
            };
            let right_text = if is_selected {
                format!(" {} {}", desc_preview, cat_tag)
            } else {
                format!(" {}", desc_preview)
            };
            let consumed = display_label.len() + 2; // icon + space + label
            let remaining = (content_width as usize + 2) // inside padding
                .saturating_sub(consumed + right_text.len() + 1);
            let padding = " ".repeat(remaining.max(1));

            write!(stdout, "{}{}", padding, right_text)?;

            if is_selected {
                stdout.queue(ResetColor)?;
            }
        } else {
            // Empty line
            let padding = " ".repeat(content_width as usize);
            stdout.queue(Print(&padding))?;
        }

        stdout.queue(Print(" │"))?;
    }

    // ── Footer ──
    let footer_y = top + 1 + body_height;
    stdout.queue(MoveTo(left, footer_y))?;
    stdout.queue(Print("└─ "))?;

    let help_text = " ↑↓ navigate · PageUp/Down · Home/End · /q or Esc cancel · Enter select ";
    let help_remaining = content_width.saturating_sub(help_text.len() as u16 + 2);
    let help_padding = "─".repeat(help_remaining.max(1) as usize);
    stdout
        .queue(SetForegroundColor(Color::DarkGrey))?
        .queue(Print(help_text))?
        .queue(ResetColor)?
        .queue(Print(&help_padding))?
        .queue(Print("─┘"))?;

    stdout.flush()
}
