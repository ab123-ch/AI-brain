//! Terminal progress display for AI Brain CLI.
//!
//! Renders real-time progress events to the terminal using Braille spinner
//! animation and color-coded status indicators with Chinese brain names.

use std::io::Write;

use brain_core::types::ProgressEvent;

// ---------------------------------------------------------------------------
// Braille Spinner Animation
// ---------------------------------------------------------------------------

/// 10-frame Braille spinner characters
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⼾", "⼿", "⾀", "⾁", "⾂", "⾃"];

// ---------------------------------------------------------------------------
// Color helpers (ANSI escape codes)
// ---------------------------------------------------------------------------

const BLUE: &str = "\x1b[34m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";
const ERASE_LINE: &str = "\x1b[2K\r";
const SAVE_CURSOR: &str = "\x1b[s";

// ---------------------------------------------------------------------------
// Brain name mapping (English ID → Chinese display name)
// ---------------------------------------------------------------------------

/// Map brain identifier to Chinese display name.
pub fn brain_display_name(brain: &str) -> &str {
    match brain {
        "main" => "主脑",
        "memory" => "记忆脑",
        "eval" => "评估脑",
        _ => brain,
    }
}

// ---------------------------------------------------------------------------
// Display State
// ---------------------------------------------------------------------------

/// Current display state for the progress UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DisplayState {
    /// Not showing anything
    Idle,
    /// Showing a spinner with a message
    Spinning,
    /// Streaming text content
    Streaming,
}

// ---------------------------------------------------------------------------
// ProgressDisplay
// ---------------------------------------------------------------------------

/// Terminal progress display driven by `ProgressEvent`s.
///
/// Usage:
/// ```ignore
/// let mut display = ProgressDisplay::new();
/// display.handle_event(&event);
/// display.tick(); // call periodically (every 150ms) for spinner animation
/// ```
pub struct ProgressDisplay {
    state: DisplayState,
    spinner_frame: usize,
    /// Current spinner label
    spinner_label: String,
    /// Whether the last output was a spinner line (needs erasing)
    has_spinner_line: bool,
}

impl ProgressDisplay {
    pub fn new() -> Self {
        Self {
            state: DisplayState::Idle,
            spinner_frame: 0,
            spinner_label: String::new(),
            has_spinner_line: false,
        }
    }

    /// Handle a progress event, updating the terminal display.
    pub fn handle_event(&mut self, event: &ProgressEvent) {
        match event {
            ProgressEvent::Connecting { brain, model } => {
                let name = brain_display_name(brain);
                self.transition_to_spinner(&format!("{name}-连接中... ({model})"));
            }
            ProgressEvent::Thinking { brain } => {
                let name = brain_display_name(brain);
                self.transition_to_spinner(&format!("{name}-推理中..."));
            }
            ProgressEvent::TextDelta { text } => {
                self.transition_to_streaming();
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            ProgressEvent::ToolStart {
                brain,
                tool_name,
                input,
            } => {
                let name = brain_display_name(brain);
                let preview = truncate_str(input, 40);
                self.transition_to_spinner(&format!("{name}-调用工具: {tool_name}({preview})"));
            }
            ProgressEvent::ToolDone {
                brain,
                tool_name,
                duration_ms,
                output_preview,
                is_error,
            } => {
                self.erase_spinner_line();
                let name = brain_display_name(brain);
                let dur = format_duration(*duration_ms);
                if *is_error {
                    let preview = truncate_str(output_preview, 60);
                    println!("{YELLOW}  {name} ✘ {tool_name} ({dur}) — {preview}{RESET}");
                } else {
                    println!("{GREEN}  {name} ✔ {tool_name} ({dur}){RESET}");
                }
                self.state = DisplayState::Idle;
                self.has_spinner_line = false;
            }
            ProgressEvent::MemoryInjected { count, preview } => {
                self.erase_spinner_line();
                let p = truncate_str(preview, 50);
                println!("{CYAN}  🧠 记忆注入: {count} 条 — {p}{RESET}");
                self.state = DisplayState::Idle;
                self.has_spinner_line = false;
            }
            ProgressEvent::EvaluationStart => {
                self.transition_to_spinner("评估脑-检查中...");
            }
            ProgressEvent::EvaluationResult { passed, issues } => {
                self.erase_spinner_line();
                if *passed {
                    println!("{GREEN}  评估 ✔ 通过{RESET}");
                } else {
                    let n = issues.len();
                    println!("{YELLOW}  评估 ✘ 发现 {n} 个问题{RESET}");
                    for issue in issues {
                        println!("{DIM}    - {issue}{RESET}");
                    }
                }
                self.state = DisplayState::Idle;
                self.has_spinner_line = false;
            }
            ProgressEvent::Evaluating => {
                self.transition_to_spinner("主脑-评估答案中...");
            }
            ProgressEvent::LlmRetry {
                attempt,
                max_attempts,
                error,
            } => {
                self.erase_spinner_line();
                println!("{YELLOW}  LLM 调用失败 ({attempt}/{max_attempts}): {error}{RESET}");
                println!("{DIM}  60 秒后重试... (输入 q 取消){RESET}");
                self.state = DisplayState::Idle;
                self.has_spinner_line = false;
            }
            ProgressEvent::Done => {
                self.erase_spinner_line();
                println!();
                self.state = DisplayState::Idle;
                self.has_spinner_line = false;
            }
        }
    }

    /// Advance the spinner animation by one frame.
    ///
    /// Call this periodically (e.g. every 150ms) from a timer tick.
    pub fn tick(&mut self) {
        if self.state != DisplayState::Spinning {
            return;
        }

        self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
        let frame = SPINNER_FRAMES[self.spinner_frame];

        // Overwrite the current line
        print!(
            "{ERASE_LINE}{SAVE_CURSOR}{BLUE}{frame}{RESET} {CYAN}{}{RESET}",
            self.spinner_label
        );
        let _ = std::io::stdout().flush();
        self.has_spinner_line = true;
    }

    // ─── Internal helpers ───────────────────────────────────────────

    /// Transition to spinner mode with the given label.
    fn transition_to_spinner(&mut self, label: &str) {
        // If we were streaming text, just go to a new line for the spinner
        if self.state == DisplayState::Streaming {
            println!();
        }
        self.state = DisplayState::Spinning;
        self.spinner_label = label.to_string();
        self.spinner_frame = 0;

        // Draw first frame immediately
        let frame = SPINNER_FRAMES[0];
        print!("{ERASE_LINE}{SAVE_CURSOR}{BLUE}{frame}{RESET} {CYAN}{label}{RESET}");
        let _ = std::io::stdout().flush();
        self.has_spinner_line = true;
    }

    /// Transition to streaming mode (text output).
    fn transition_to_streaming(&mut self) {
        if self.state == DisplayState::Spinning {
            self.erase_spinner_line();
        }
        self.state = DisplayState::Streaming;
    }

    /// Erase the current spinner line.
    fn erase_spinner_line(&mut self) {
        if self.has_spinner_line {
            print!("{ERASE_LINE}");
            let _ = std::io::stdout().flush();
            self.has_spinner_line = false;
        }
    }
}

impl Default for ProgressDisplay {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Format a duration in milliseconds to a human-readable string.
fn format_duration(duration_ms: u64) -> String {
    if duration_ms < 1000 {
        format!("{duration_ms}ms")
    } else {
        let secs = duration_ms / 1000;
        let frac = duration_ms % 1000 / 100;
        format!("{secs}.{frac}s")
    }
}

/// Truncate a string to `max` characters, appending "…" if truncated.
fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = s
            .char_indices()
            .take(max)
            .last()
            .map_or(0, |(i, c)| i + c.len_utf8());
        format!("{}…", &s[..end])
    }
}
