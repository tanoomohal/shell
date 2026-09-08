//! A single terminal session: the emulator state plus the pty feeding it.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Arc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, Notify, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop as PtyEventLoop, Msg, Notifier};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty;
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
            shell: None,
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

    /// Re-reads the tab's last meaningful line of output. Returns whether it
    /// changed.
    ///
    /// Scans the viewport bottom-up and takes the first line that survives
    /// `summarize_line`. The cursor's own line is skipped: for a shell sitting
    /// at an empty prompt that line *is* the prompt, and the useful summary is
    /// the output above it.
    pub fn refresh_summary(&mut self) -> bool {
        let next = {
            let term = self.term.lock();
            let grid = term.grid();
            let cursor_line = grid.cursor.point.line;
            let columns = grid.columns();

            let mut found = None;
            for index in (0..grid.screen_lines()).rev() {
                let line = Line(index as i32);
                if line == cursor_line {
                    continue;
                }
                let row = &grid[line];
                let mut raw = String::with_capacity(columns);
                for column in 0..columns {
                    raw.push(row[Column(column)].c);
                }
                if let Some(summary) = summarize_line(&raw) {
                    found = Some(summary);
                    break;
                }
            }
            found.unwrap_or_default()
        };

        if next == self.summary {
            return false;
        }
        self.summary = next;
        true
    }

    pub fn mark_output(&mut self) {
        self.tracker.mark_output();
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
        changed |= self.refresh_summary();
        let activity = self.tracker.activity(self.label.kind());

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
