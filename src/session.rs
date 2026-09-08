//! A single terminal session: the emulator state plus the pty feeding it.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Arc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, Notify, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop as PtyEventLoop, Msg, Notifier};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty::{self, Shell as PtyShell};
use winit::event_loop::EventLoopProxy;

use crate::agent::{self, Activity, ActivityTracker, AgentKind, LabelTracker};

/// Terminal events are produced on the pty reader thread, so they are funneled
/// into winit's loop as user events to keep all state mutation single-threaded.
#[derive(Debug)]
pub struct UserEvent {
    pub session: u64,
    pub event: TermEvent,
}

#[derive(Clone)]
pub struct EventProxy {
    id: u64,
    proxy: EventLoopProxy<UserEvent>,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: TermEvent) {
        let _ = self.proxy.send_event(UserEvent {
            session: self.id,
            event,
        });
    }
}

/// Grid dimensions in cells. `alacritty_terminal` asks for this via
/// `Dimensions`; scrollback is owned by the grid, so `total_lines` only needs
/// to describe the viewport at construction time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermSize {
    pub columns: usize,
    pub screen_lines: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

pub struct Session {
    pub id: u64,
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    pub notifier: Notifier,
    /// The pty master fd, kept so the foreground process group can be polled.
    /// The fd itself is owned by the pty living on the reader thread.
    pub master_fd: RawFd,
    pub size: TermSize,
    /// Title the program set via OSC. Deliberately *not* used as the tab
    /// label: a shell can set it to anything, which makes it a much worse
    /// signal than the foreground process.
    pub title: String,
    /// Decides the tab's label from foreground-process readings.
    label: LabelTracker,
    /// Last meaningful line of output, shown under the tab's label.
    summary: String,
    /// Number of sub-agents the tool in this tab reports running, scraped
    /// from its own status line.
    agent_count: Option<u32>,
    pub activity: Activity,
    /// Set when a background tab starts waiting on the user, cleared when the
    /// tab is next focused.
    pub needs_attention: bool,
    tracker: ActivityTracker,
}

impl Session {
    pub fn new(
        id: u64,
        size: TermSize,
        window_size: WindowSize,
        proxy: &EventLoopProxy<UserEvent>,
    ) -> io::Result<Self> {
        let pty_options = tty::Options {
            shell: startup_command(),
            working_directory: dirs::home_dir(),
            drain_on_exit: false,
            env: HashMap::new(),
            ..Default::default()
        };

        let pty = tty::new(&pty_options, window_size, id)?;
        let master_fd = pty.file().as_raw_fd();

        let event_proxy = EventProxy {
            id,
            proxy: proxy.clone(),
        };

        let config = TermConfig {
            scrolling_history: 10_000,
            ..Default::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(
            config,
            &size,
            event_proxy.clone(),
        )));

        let pty_loop = PtyEventLoop::new(term.clone(), event_proxy, pty, false, false)?;
        let notifier = Notifier(pty_loop.channel());
        pty_loop.spawn();

        Ok(Self {
            id,
            term,
            notifier,
            master_fd,
            size,
            title: String::new(),
            label: LabelTracker::new(shell_name()),
            summary: String::new(),
            agent_count: None,
            activity: Activity::Idle,
            needs_attention: false,
            tracker: ActivityTracker::default(),
        })
    }

    pub fn write(&self, bytes: Vec<u8>) {
        self.notifier.notify(bytes);
    }

    pub fn resize(&mut self, size: TermSize, window_size: WindowSize) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.term.lock().resize(size);
        self.notifier.on_resize(window_size);
    }

    /// The label shown in the sidebar.
    pub fn label(&self) -> &str {
        self.label.name()
    }

    pub fn agent(&self) -> AgentKind {
        self.label.kind()
    }

    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// Text of one grid row, for link detection. Empty if that line has
    /// scrolled out of the buffer since the caller resolved it.
    pub fn row_text(&self, line: Line) -> String {
        let term = self.term.lock();
        let grid = term.grid();
        if line < grid.topmost_line() || line >= Line(grid.screen_lines() as i32) {
            return String::new();
        }
        let row = &grid[line];
        (0..grid.columns()).map(|c| row[Column(c)].c).collect()
    }

    pub fn agent_count(&self) -> Option<u32> {
        self.agent_count
    }

    /// Reads everything the sidebar needs from the grid in one pass:
    /// the summary line, a change fingerprint, and any sub-agent count the
    /// running tool advertises.
    ///
    /// Returns whether anything displayed changed.
    fn refresh_from_grid(&mut self) -> (bool, u64) {
        let (summary, fingerprint, agent_count) = {
            let term = self.term.lock();
            let grid = term.grid();
            let cursor_line = grid.cursor.point.line;
            let columns = grid.columns();

            let mut hasher = DefaultHasher::new();
            let mut rows: Vec<String> = Vec::with_capacity(grid.screen_lines());
            for index in 0..grid.screen_lines() {
                let row = &grid[Line(index as i32)];
                let mut raw = String::with_capacity(columns);
                for column in 0..columns {
                    raw.push(row[Column(column)].c);
                }
                for c in raw.chars() {
                    if let Some(normalized) = fingerprint_char(c) {
                        normalized.hash(&mut hasher);
                    }
                }
                rows.push(raw);
            }

            // Both the summary and the status line live near the bottom.
            let mut summary = None;
            let mut agent_count = None;
            for index in (0..rows.len()).rev() {
                if agent_count.is_none() {
                    agent_count = parse_agent_count(&rows[index]);
                }
                if summary.is_none() && Line(index as i32) != cursor_line {
                    summary = summarize_line(&rows[index]);
                }
                if summary.is_some() && agent_count.is_some() {
                    break;
                }
            }

            (summary.unwrap_or_default(), hasher.finish(), agent_count)
        };

        let mut changed = false;
        if summary != self.summary {
            self.summary = summary;
            changed = true;
        }
        if agent_count != self.agent_count {
            self.agent_count = agent_count;
            changed = true;
        }
        (changed, fingerprint)
    }

    /// Re-reads the pty's foreground process and recomputes activity.
    /// Returns whether anything the sidebar displays changed.
    pub fn poll_state(&mut self, is_active: bool) -> bool {
        // An unreadable foreground process (a privileged one, or a race with
        // process exit) must not clear a good label, so keep the last one.
        let Some(name) = agent::foreground_process_name(self.master_fd) else {
            return false;
        };
        let mut changed = self.label.observe(&name);
        let (grid_changed, fingerprint) = self.refresh_from_grid();
        changed |= grid_changed;
        let activity = self.tracker.observe(fingerprint, self.label.kind());

        if activity != self.activity {
            // Only a fresh transition into Waiting raises attention, so
            // returning to an already-waiting tab doesn't re-flag it.
            if activity == Activity::Waiting && !is_active {
                self.needs_attention = true;
            }
            self.activity = activity;
            changed = true;
        }
        if is_active && self.needs_attention {
            self.needs_attention = false;
            changed = true;
        }
        changed
    }

    pub fn flag_attention(&mut self) {
        self.needs_attention = true;
    }

    pub fn shutdown(&self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}

/// Basename of the user's shell, e.g. `zsh`.
fn shell_name() -> String {
    std::env::var("SHELL")
        .ok()
        .and_then(|path| {
            std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "sh".to_string())
}

/// Longest summary shown in the sidebar, in characters.
const SUMMARY_MAX_CHARS: usize = 52;

/// Characters that carry no meaning in a summary: rules, borders, spinner
/// frames, bullets, and prompt markers.
fn is_decoration(c: char) -> bool {
    matches!(c,
        '\u{2190}'..='\u{21ff}'   // arrows
        | '\u{2500}'..='\u{257f}' // box drawing
        | '\u{2580}'..='\u{259f}' // block elements
        | '\u{25a0}'..='\u{25ff}' // geometric shapes
        | '\u{2800}'..='\u{28ff}' // braille, used for spinners
        | '\u{2022}' | '\u{00b7}' | '\u{2039}' | '\u{203a}' | '\u{00ab}' | '\u{00bb}'
        | '>' | '<' | '$' | '%' | '#' | '*' | '|' | '=' | '-' | '_' | '+' | '~'
    )
}

/// Reduces one grid row to a summary, or `None` if it carries nothing worth
/// showing.
fn summarize_line(raw: &str) -> Option<String> {
    let cleaned = raw.trim_matches(|c: char| is_decoration(c) || c.is_whitespace());
    if cleaned.is_empty() {
        return None;
    }

    // Collapse internal whitespace runs, since grid rows are space-padded and
    // TUIs align with wide gaps.
    let mut out = String::with_capacity(cleaned.len());
    let mut in_space = false;
    for c in cleaned.chars() {
        if c.is_whitespace() {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    let out = out.trim();

    // Require some actual words, so rules and lone punctuation are rejected.
    if out.chars().filter(|c| c.is_alphanumeric()).count() < 3 {
        return None;
    }

    if out.chars().count() > SUMMARY_MAX_CHARS {
        let mut truncated: String = out.chars().take(SUMMARY_MAX_CHARS - 1).collect();
        truncated.push('\u{2026}');
        return Some(truncated);
    }
    Some(out.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_and_padded_rows_are_rejected() {
        assert_eq!(summarize_line(""), None);
        assert_eq!(summarize_line("                    "), None);
    }

    #[test]
    fn rules_and_borders_are_rejected() {
        assert_eq!(summarize_line("\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}"), None);
        assert_eq!(summarize_line("------------"), None);
        assert_eq!(summarize_line("| |"), None);
    }

    #[test]
    fn spinner_frames_are_stripped() {
        // A braille spinner plus a status message.
        assert_eq!(
            summarize_line("\u{28f7} Building project").as_deref(),
            Some("Building project")
        );
    }

    #[test]
    fn prompt_markers_are_stripped() {
        assert_eq!(summarize_line("> npm test").as_deref(), Some("npm test"));
        assert_eq!(
            summarize_line("$ cargo build --release").as_deref(),
            Some("cargo build --release")
        );
    }

    #[test]
    fn interior_whitespace_collapses() {
        assert_eq!(
            summarize_line("  tests      42 passed   ").as_deref(),
            Some("tests 42 passed")
        );
    }

    #[test]
    fn long_lines_are_truncated_with_an_ellipsis() {
        let long = "a".repeat(200);
        let out = summarize_line(&long).unwrap();
        assert_eq!(out.chars().count(), SUMMARY_MAX_CHARS);
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn box_drawn_status_lines_survive() {
        assert_eq!(
            summarize_line("\u{2502} Waiting for your input \u{2502}").as_deref(),
            Some("Waiting for your input")
        );
    }
}

/// Program a new tab launches instead of the login shell.
///
/// Read from `SHELL_COMMAND`, split on whitespace. This is the hook the
/// settings UI will eventually write to, and it is what makes agent detection
/// testable without driving the GUI.
fn startup_command() -> Option<PtyShell> {
    let raw = std::env::var("SHELL_COMMAND").ok()?;
    let mut parts = raw.split_whitespace().map(str::to_string);
    let program = parts.next()?;
    Some(PtyShell::new(program, parts.collect()))
}

/// Normalizes one grid character for the change fingerprint, or drops it.
///
/// Everything that an idle-but-repainting TUI animates is removed: spinner
/// frames and rules are decoration, layout churn is whitespace, and elapsed
/// timers and token counters are digits. What survives is the text content, so
/// a fingerprint only moves when something the user would call progress does.
fn fingerprint_char(c: char) -> Option<char> {
    if c.is_whitespace() || is_decoration(c) {
        return None;
    }
    if c.is_ascii_digit() {
        return Some('0');
    }
    Some(c)
}

/// Scrapes a sub-agent count out of a tool's own status line, e.g. the
/// `1 agent` that Claude Code prints while a subagent is running.
fn parse_agent_count(line: &str) -> Option<u32> {
    let lower = line.to_ascii_lowercase();
    let mut found = None;
    let mut cursor = 0;

    while let Some(offset) = lower[cursor..].find("agent") {
        let at = cursor + offset;
        cursor = at + "agent".len();

        // Require a word boundary, so "agentic" doesn't match.
        let next = lower[cursor..].chars().next();
        let boundary = matches!(next, None | Some('s'))
            || next.is_some_and(|c| !c.is_alphanumeric());
        if !boundary {
            continue;
        }

        // Walk back over the separator, then collect the digits before it.
        let digits: String = lower[..at]
            .trim_end()
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect();
        if digits.is_empty() {
            continue;
        }
        if let Ok(count) = digits.chars().rev().collect::<String>().parse::<u32>() {
            found = Some(count);
        }
    }

    found
}
