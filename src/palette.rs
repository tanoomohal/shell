//! Arc/Dia-style visual fuzzy command and history palette overlay.
//!
//! Designed around coding agents (Claude, Codex, Antigravity) as the primary
//! reason for this terminal client, alongside instant fuzzy shell history search
//! and terminal actions.

use alacritty_terminal::grid::Dimensions;
use crate::agent::AgentKind;
use crate::session::Session;
use crate::theme::Rgb8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteMode {
    /// Full launcher & controller: Agent quick-launch, tab switcher, agent prompts,
    /// command history, and terminal actions. Triggered via Cmd+P.
    Commands,
    /// Dedicated visual fuzzy history search. Triggered via Cmd+R.
    History,
    /// Tab Inspector modal. Triggered via Cmd+I.
    Inspector,
    /// Tab Title rename modal. Triggered via Shift+Cmd+I.
    EditTitle,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaletteAction {
    /// Write string directly into the active PTY (e.g. command or prompt).
    /// If `execute` is true, also appends `\r` to submit immediately.
    WriteToPty { text: String, execute: bool },
    /// Switch directly to an existing session tab.
    SwitchTab(usize),
    /// Spawn a new tab running an agent CLI or shell.
    SpawnAgent { name: String, command: Option<String> },
    /// Rename tab title.
    SetTitle(String),
    /// Terminal buffer action.
    ClearScrollback,
    FindInBuffer,
    Zoom(f32),
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteCategory {
    Agent,      // 🤖 Launch Agent CLI
    RunningTab, // ⚡ Running Agent Sessions
    Prompt,     // 💬 Quick Agent Commands (/plan, /review, etc.)
    History,    // 📜 Shell History Search
    Action,     // ⚙️ Terminal Control
    Info,       // ℹ️ Inspector Info
}

#[derive(Clone, Debug)]
pub struct PaletteItem {
    pub title: String,
    pub subtitle: Option<String>,
    pub badge: Option<String>,
    pub icon_color: Option<Rgb8>,
    pub category: PaletteCategory,
    pub action: PaletteAction,
}

pub struct PaletteState {
    pub mode: PaletteMode,
    pub query: String,
    pub cursor_pos: usize,
    pub selected: usize,
    pub history_items: Vec<String>,
    pub all_items: Vec<PaletteItem>,
    pub filtered_items: Vec<PaletteItem>,
}

impl PaletteState {
    pub fn new(mode: PaletteMode) -> Self {
        let history_items = load_shell_history();
        Self {
            mode,
            query: String::new(),
            cursor_pos: 0,
            selected: 0,
            history_items,
            all_items: Vec::new(),
            filtered_items: Vec::new(),
        }
    }

    /// Rebuilds the item catalogue based on current running sessions and mode,
    /// then reapplies query filtering.
    pub fn update_items(&mut self, sessions: &[Session], active_tab: usize) {
        let mut items = Vec::with_capacity(32 + self.history_items.len());

        match self.mode {
            PaletteMode::Commands => {
                // 1. Running Agent Tabs (quick switch, prioritized if waiting)
                for (index, session) in sessions.iter().enumerate() {
                    let is_active = index == active_tab;
                    let dot_color = session.agent().dot_color();

                    let status = if session.needs_attention {
                        "Waiting for your input 🔔".to_string()
                    } else if !session.summary().is_empty() {
                        session.summary().to_string()
                    } else if is_active {
                        "Active Tab".to_string()
                    } else {
                        "Running in background".to_string()
                    };

                    let badge = if session.needs_attention {
                        Some("ATTENTION".to_string())
                    } else if let Some(n) = session.agent_count() {
                        Some(format!("{n} agent{}", if n == 1 { "" } else { "s" }))
                    } else if is_active {
                        Some("CURRENT".to_string())
                    } else {
                        None
                    };

                    items.push(PaletteItem {
                        title: format!("Tab {}: {}", index + 1, session.label()),
                        subtitle: Some(status),
                        badge,
                        icon_color: Some(dot_color),
                        category: PaletteCategory::RunningTab,
                        action: PaletteAction::SwitchTab(index),
                    });
                }

                // 2. Agent Launchers (Claude Code, Antigravity, Codex, Shell)
                items.push(PaletteItem {
                    title: "Launch Claude Code".to_string(),
                    subtitle: Some("Autonomous coding agent (claude)".to_string()),
                    badge: Some("AGENT".to_string()),
                    icon_color: Some(AgentKind::Claude.dot_color()),
                    category: PaletteCategory::Agent,
                    action: PaletteAction::SpawnAgent {
                        name: "claude".to_string(),
                        command: Some("claude".to_string()),
                    },
                });

                items.push(PaletteItem {
                    title: "Launch Antigravity Agent".to_string(),
                    subtitle: Some("DeepMind agentic coding CLI (agy)".to_string()),
                    badge: Some("AGENT".to_string()),
                    icon_color: Some(AgentKind::Antigravity.dot_color()),
                    category: PaletteCategory::Agent,
                    action: PaletteAction::SpawnAgent {
                        name: "antigravity".to_string(),
                        command: Some("agy".to_string()),
                    },
                });

                items.push(PaletteItem {
                    title: "Launch Codex CLI".to_string(),
                    subtitle: Some("Codex coding agent (codex)".to_string()),
                    badge: Some("AGENT".to_string()),
                    icon_color: Some(AgentKind::Codex.dot_color()),
                    category: PaletteCategory::Agent,
                    action: PaletteAction::SpawnAgent {
                        name: "codex".to_string(),
                        command: Some("codex".to_string()),
                    },
                });

                items.push(PaletteItem {
                    title: "New Shell Tab".to_string(),
                    subtitle: Some("Standard interactive login shell".to_string()),
                    badge: Some("SHELL".to_string()),
                    icon_color: Some(AgentKind::Shell.dot_color()),
                    category: PaletteCategory::Agent,
                    action: PaletteAction::SpawnAgent {
                        name: "shell".to_string(),
                        command: None,
                    },
                });

                // 3. Agent Prompts / Slash Commands for active terminal
                items.push(PaletteItem {
                    title: "/plan".to_string(),
                    subtitle: Some("Ask agent for step-by-step implementation plan".to_string()),
                    badge: Some("PROMPT".to_string()),
                    icon_color: Some([0x6c, 0x9e, 0xd9]), // blue
                    category: PaletteCategory::Prompt,
                    action: PaletteAction::WriteToPty {
                        text: "/plan ".to_string(),
                        execute: false,
                    },
                });

                items.push(PaletteItem {
                    title: "/review".to_string(),
                    subtitle: Some("Ask agent to review git diff & uncommitted changes".to_string()),
                    badge: Some("PROMPT".to_string()),
                    icon_color: Some([0x8f, 0xc7, 0x7a]), // green
                    category: PaletteCategory::Prompt,
                    action: PaletteAction::WriteToPty {
                        text: "/review\r".to_string(),
                        execute: true,
                    },
                });

                items.push(PaletteItem {
                    title: "/goal".to_string(),
                    subtitle: Some("Run autonomous goal-driven task loop".to_string()),
                    badge: Some("PROMPT".to_string()),
                    icon_color: Some([0xe0, 0xb5, 0x5f]), // yellow
                    category: PaletteCategory::Prompt,
                    action: PaletteAction::WriteToPty {
                        text: "/goal ".to_string(),
                        execute: false,
                    },
                });

                items.push(PaletteItem {
                    title: "Interrupt Agent / Cancel Process".to_string(),
                    subtitle: Some("Send SIGINT (Ctrl+C) to cancel active turn".to_string()),
                    badge: Some("SIGNAL".to_string()),
                    icon_color: Some([0xe5, 0x5f, 0x5f]), // red
                    category: PaletteCategory::Prompt,
                    action: PaletteAction::WriteToPty {
                        text: "\x03".to_string(),
                        execute: false,
                    },
                });

                // 4. Terminal Actions
                items.push(PaletteItem {
                    title: "Clear Screen & Scrollback".to_string(),
                    subtitle: Some("Reset terminal display (Cmd+K)".to_string()),
                    badge: Some("TERMINAL".to_string()),
                    icon_color: None,
                    category: PaletteCategory::Action,
                    action: PaletteAction::ClearScrollback,
                });

                items.push(PaletteItem {
                    title: "Find in Buffer".to_string(),
                    subtitle: Some("Search terminal scrollback (Cmd+F)".to_string()),
                    badge: Some("TERMINAL".to_string()),
                    icon_color: None,
                    category: PaletteCategory::Action,
                    action: PaletteAction::FindInBuffer,
                });

                items.push(PaletteItem {
                    title: "Zoom Font In".to_string(),
                    subtitle: Some("Increase terminal font size (Cmd++)".to_string()),
                    badge: None,
                    icon_color: None,
                    category: PaletteCategory::Action,
                    action: PaletteAction::Zoom(1.0),
                });

                items.push(PaletteItem {
                    title: "Zoom Font Out".to_string(),
                    subtitle: Some("Decrease terminal font size (Cmd+-)".to_string()),
                    badge: None,
                    icon_color: None,
                    category: PaletteCategory::Action,
                    action: PaletteAction::Zoom(-1.0),
                });

                // 5. Recent History
                for cmd in self.history_items.iter().take(200) {
                    items.push(PaletteItem {
                        title: cmd.clone(),
                        subtitle: Some("Execute command in active tab".to_string()),
                        badge: Some("HISTORY".to_string()),
                        icon_color: None,
                        category: PaletteCategory::History,
                        action: PaletteAction::WriteToPty {
                            text: cmd.clone(),
                            execute: true,
                        },
                    });
                }
            },

            PaletteMode::History => {
                // Dedicated visual history search mode
                for cmd in &self.history_items {
                    items.push(PaletteItem {
                        title: cmd.clone(),
                        subtitle: Some("Execute command in active tab".to_string()),
                        badge: Some("HISTORY".to_string()),
                        icon_color: None,
                        category: PaletteCategory::History,
                        action: PaletteAction::WriteToPty {
                            text: cmd.clone(),
                            execute: true,
                        },
                    });
                }
            },

            PaletteMode::Inspector => {
                if let Some(session) = sessions.get(active_tab) {
                    let term = session.term.lock();
                    let grid = term.grid();
                    let total_lines = grid.total_lines();
                    let cursor = grid.cursor.point;
                    let display_offset = grid.display_offset();
                    drop(term);

                    items.push(PaletteItem {
                        title: format!("Tab Name: {}", session.label()),
                        subtitle: Some(format!(
                            "Custom title: {}",
                            session.custom_title.as_deref().unwrap_or("None (auto-derived)")
                        )),
                        badge: Some("TITLE".to_string()),
                        icon_color: Some(session.agent().dot_color()),
                        category: PaletteCategory::Info,
                        action: PaletteAction::None,
                    });

                    items.push(PaletteItem {
                        title: format!(
                            "Terminal Dimensions: {} cols × {} rows",
                            session.size.columns, session.size.screen_lines
                        ),
                        subtitle: Some(format!(
                            "Total scrollback: {} lines (offset: {})",
                            total_lines, display_offset
                        )),
                        badge: Some("GRID".to_string()),
                        icon_color: Some([0x6c, 0x9e, 0xd9]),
                        category: PaletteCategory::Info,
                        action: PaletteAction::None,
                    });

                    items.push(PaletteItem {
                        title: format!("Cursor Position: Row {}, Col {}", cursor.line.0, cursor.column.0),
                        subtitle: Some("Zero-indexed buffer coordinates".to_string()),
                        badge: Some("CURSOR".to_string()),
                        icon_color: Some([0x8f, 0xc7, 0x7a]),
                        category: PaletteCategory::Info,
                        action: PaletteAction::None,
                    });

                    items.push(PaletteItem {
                        title: format!("Agent Status: {:?}", session.agent()),
                        subtitle: Some(if session.needs_attention {
                            "Needs attention 🔔 (waiting on user)".to_string()
                        } else if !session.summary().is_empty() {
                            session.summary().to_string()
                        } else {
                            "Idle / in normal shell loop".to_string()
                        }),
                        badge: Some(
                            if session.needs_attention {
                                "ATTENTION"
                            } else {
                                "STATUS"
                            }
                            .to_string(),
                        ),
                        icon_color: Some(if session.needs_attention {
                            [0xe5, 0x5f, 0x5f]
                        } else {
                            [0x8f, 0xc7, 0x7a]
                        }),
                        category: PaletteCategory::Info,
                        action: PaletteAction::None,
                    });

                    items.push(PaletteItem {
                        title: format!("PTY Process FD: {}", session.master_fd),
                        subtitle: Some(format!(
                            "OSC Title: {}",
                            if session.title.is_empty() {
                                "(empty)"
                            } else {
                                &session.title
                            }
                        )),
                        badge: Some("PTY".to_string()),
                        icon_color: Some([0xb5, 0x7e, 0xdc]),
                        category: PaletteCategory::Info,
                        action: PaletteAction::None,
                    });
                }
            },

            PaletteMode::EditTitle => {
                let current_label = sessions.get(active_tab).map(|s| s.label()).unwrap_or("Tab");
                let title_to_set = if self.query.trim().is_empty() {
                    current_label.to_string()
                } else {
                    self.query.trim().to_string()
                };
                items.push(PaletteItem {
                    title: format!("Set Tab Title to: \"{title_to_set}\""),
                    subtitle: Some("Press Enter to save, or Esc to cancel".to_string()),
                    badge: Some("RENAME".to_string()),
                    icon_color: Some([0x8f, 0xc7, 0x7a]),
                    category: PaletteCategory::Action,
                    action: PaletteAction::SetTitle(title_to_set),
                });
                if sessions
                    .get(active_tab)
                    .and_then(|s| s.custom_title.as_ref())
                    .is_some()
                {
                    items.push(PaletteItem {
                        title: "Reset to default auto-derived title".to_string(),
                        subtitle: Some("Clear custom title and track process name".to_string()),
                        badge: Some("RESET".to_string()),
                        icon_color: Some([0xe0, 0xb5, 0x5f]),
                        category: PaletteCategory::Action,
                        action: PaletteAction::SetTitle(String::new()),
                    });
                }
            },
        }

        self.all_items = items;
        self.filter();
    }

    /// Filters all items against the current search query.
    pub fn filter(&mut self) {
        let q = self.query.trim();

        if self.mode == PaletteMode::EditTitle {
            self.filtered_items = self.all_items.clone();
            if let Some(item) = self.filtered_items.first_mut() {
                let title_to_set = if q.is_empty() { "Untitled" } else { q };
                item.title = format!("Set Tab Title to: \"{title_to_set}\"");
                item.action = PaletteAction::SetTitle(q.to_string());
            }
            self.selected = 0;
            return;
        }

        if q.is_empty() {
            self.filtered_items = self.all_items.clone();
        } else {
            let mut matched = Vec::new();

            if self.mode == PaletteMode::Commands {
                // If user typed a custom command, let them run it directly
                matched.push(PaletteItem {
                    title: format!("Run: {q}"),
                    subtitle: Some("Execute directly in active terminal".to_string()),
                    badge: Some("RUN".to_string()),
                    icon_color: Some([0x5e, 0xe6, 0xb8]), // emerald
                    category: PaletteCategory::Action,
                    action: PaletteAction::WriteToPty {
                        text: q.to_string(),
                        execute: true,
                    },
                });
            }

            for item in &self.all_items {
                if fuzzy_match(&item.title, q)
                    || item
                        .subtitle
                        .as_deref()
                        .map(|s| s.to_lowercase().contains(&q.to_lowercase()))
                        .unwrap_or(false)
                {
                    matched.push(item.clone());
                }
            }

            self.filtered_items = matched;
        }

        // Clamp selection
        if self.filtered_items.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered_items.len() {
            self.selected = self.filtered_items.len() - 1;
        }
    }

    // ── Input & Navigation ───────────────────────────────────────────

    pub fn insert_char(&mut self, c: char) {
        self.query.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
        self.filter();
    }

    pub fn backspace(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let prev = self.query[..self.cursor_pos]
            .char_indices()
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.query.drain(prev..self.cursor_pos);
        self.cursor_pos = prev;
        self.filter();
    }

    pub fn delete_word(&mut self) {
        if self.cursor_pos == 0 {
            return;
        }
        let before = &self.query[..self.cursor_pos];
        let trimmed = before.trim_end();
        let word_start = trimmed
            .rfind(|c: char| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);
        self.query.drain(word_start..self.cursor_pos);
        self.cursor_pos = word_start;
        self.filter();
    }

    pub fn cursor_left(&mut self) {
        if let Some((i, _)) = self.query[..self.cursor_pos].char_indices().last() {
            self.cursor_pos = i;
        }
    }

    pub fn cursor_right(&mut self) {
        if self.cursor_pos < self.query.len() {
            self.cursor_pos = self.query[self.cursor_pos..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor_pos + i)
                .unwrap_or(self.query.len());
        }
    }

    pub fn cursor_home(&mut self) {
        self.cursor_pos = 0;
    }

    pub fn cursor_end(&mut self) {
        self.cursor_pos = self.query.len();
    }

    pub fn select_next(&mut self) {
        if !self.filtered_items.is_empty() {
            self.selected = (self.selected + 1) % self.filtered_items.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.filtered_items.is_empty() {
            self.selected = if self.selected == 0 {
                self.filtered_items.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    pub fn current_item(&self) -> Option<&PaletteItem> {
        self.filtered_items.get(self.selected)
    }
}

// ── Fuzzy Matching ───────────────────────────────────────────────────

/// Fast case-insensitive substring + subsequence fuzzy matching.
pub fn fuzzy_match(candidate: &str, pattern: &str) -> bool {
    if pattern.is_empty() {
        return true;
    }
    let c_lower = candidate.to_lowercase();
    let p_lower = pattern.to_lowercase();

    // 1. Direct substring check (fast path)
    if c_lower.contains(&p_lower) {
        return true;
    }

    // 2. Subsequence match (fzf style: "clac" matches "Claude Code")
    let mut p_iter = p_lower.chars();
    let mut target = match p_iter.next() {
        Some(c) => c,
        None => return true,
    };

    for c in c_lower.chars() {
        if c == target {
            match p_iter.next() {
                Some(next) => target = next,
                None => return true,
            }
        }
    }

    false
}

// ── Shell History Reader ─────────────────────────────────────────────

/// Reads recent shell history from ~/.zsh_history or ~/.bash_history.
pub fn load_shell_history() -> Vec<String> {
    let mut entries = Vec::with_capacity(300);
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return entries,
    };

    let candidates = [
        home.join(".zsh_history"),
        home.join(".bash_history"),
        home.join(".history"),
    ];

    let mut seen = std::collections::HashSet::new();

    for file in &candidates {
        if let Ok(bytes) = std::fs::read(file) {
            let text = String::from_utf8_lossy(&bytes);
            for line in text.lines().rev() {
                let clean = clean_history_line(line);
                if clean.is_empty() || clean.len() > 200 {
                    continue;
                }
                if seen.insert(clean.clone()) {
                    entries.push(clean);
                    if entries.len() >= 300 {
                        break;
                    }
                }
            }
        }
        if !entries.is_empty() {
            break;
        }
    }

    entries
}

/// Strips zsh extended metadata `: 1234567890:0;command` and escaped trailing newlines.
pub fn clean_history_line(line: &str) -> String {
    let mut s = line.trim();
    if s.starts_with(": ") {
        if let Some(semi) = s.find(';') {
            s = &s[semi + 1..];
        }
    }
    s.trim().trim_end_matches('\\').trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_zsh_extended_history() {
        assert_eq!(
            clean_history_line(": 1699999999:0;cargo run --release"),
            "cargo run --release"
        );
        assert_eq!(clean_history_line("git status"), "git status");
        assert_eq!(clean_history_line("python3 app.py\\\n"), "python3 app.py");
    }

    #[test]
    fn fuzzy_matching_works() {
        assert!(fuzzy_match("Claude Code", "claude"));
        assert!(fuzzy_match("Claude Code", "clac"));
        assert!(fuzzy_match("cargo run --release", "run"));
        assert!(fuzzy_match("cargo run --release", "crun"));
        assert!(!fuzzy_match("git push", "pull"));
    }

    #[test]
    fn navigation_wraps_correctly() {
        let mut p = PaletteState::new(PaletteMode::Commands);
        p.filtered_items = vec![
            PaletteItem {
                title: "One".to_string(),
                subtitle: None,
                badge: None,
                icon_color: None,
                category: PaletteCategory::Action,
                action: PaletteAction::ClearScrollback,
            },
            PaletteItem {
                title: "Two".to_string(),
                subtitle: None,
                badge: None,
                icon_color: None,
                category: PaletteCategory::Action,
                action: PaletteAction::ClearScrollback,
            },
        ];
        p.selected = 0;
        p.select_prev();
        assert_eq!(p.selected, 1);
        p.select_next();
        assert_eq!(p.selected, 0);
    }
}
