//! Identifies which CLI is running in a pty, and whether it wants attention.
//!
//! The foreground process group of the pty master is the reliable signal for
//! "what is the user actually talking to right now" — it follows job control,
//! so it updates when an agent starts, suspends, or exits, and it can't be
//! spoofed by a program setting its own window title.

use std::os::fd::RawFd;
use std::time::{Duration, Instant};

use crate::theme::Rgb8;

/// How long a pty has to stay quiet before an agent running in it is treated
/// as waiting on the user rather than still working.
const QUIET_BEFORE_WAITING: Duration = Duration::from_millis(700);

const SHELL_NAMES: &[&str] = &["sh", "zsh", "bash", "fish", "dash", "ksh", "tcsh", "csh", "nu"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentKind {
    Claude,
    Codex,
    Antigravity,
    Shell,
    Other,
}

impl AgentKind {
    pub fn detect(process_name: &str) -> Self {
        let name = process_name.to_ascii_lowercase();
        if name.contains("claude") {
            Self::Claude
        } else if name.contains("codex") {
            Self::Codex
        } else if name.contains("antigravity") {
            Self::Antigravity
        } else if SHELL_NAMES.contains(&name.trim_start_matches('-')) {
            Self::Shell
        } else {
            Self::Other
        }
    }

    /// Identity colors, carried over from the AppKit prototype.
    pub fn dot_color(self) -> Rgb8 {
        match self {
            Self::Claude => [0xd1, 0x78, 0x38],
            Self::Codex => [0x4c, 0xb8, 0x9e],
            Self::Antigravity => [0x8f, 0x75, 0xed],
            Self::Shell => [0x6b, 0x6b, 0x6b],
            Self::Other => [0x8a, 0x8a, 0x8a],
        }
    }

    pub fn is_agent(self) -> bool {
        matches!(self, Self::Claude | Self::Codex | Self::Antigravity)
    }
}

/// What a tab is doing, derived from output activity plus what's in the
/// foreground. Deliberately not based on parsing agent output, so it works for
/// any CLI without per-tool special casing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Activity {
    /// Nothing running but a shell sitting at its prompt.
    Idle,
    /// Output is actively streaming.
    Working,
    /// An agent is in the foreground but has gone quiet — it's your turn.
    Waiting,
}

/// Decides what to call a tab, given a stream of foreground-process readings.
///
/// Two problems make the raw foreground process a bad label on its own:
///
/// * A shell loop or build spawns a rapid succession of short-lived processes
///   (`sleep`, `date`, `git`), so the label would flicker several times a
///   second.
/// * Agent CLIs shell out constantly, so a `claude` tab would spend most of
///   its time labeled after whatever tool the agent just invoked.
///
/// So a candidate has to persist across consecutive polls before it is
/// adopted, and once a tab is identified as an agent it keeps that identity
/// until control returns to the shell.
pub struct LabelTracker {
    name: String,
    kind: AgentKind,
    pending: Option<(String, u8)>,
}

/// Consecutive polls a new name must survive before replacing the label.
const CONFIRMATIONS: u8 = 2;

impl LabelTracker {
    pub fn new(shell_name: String) -> Self {
        let kind = AgentKind::detect(&shell_name);
        Self {
            name: shell_name,
            kind,
            pending: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> AgentKind {
        self.kind
    }

    /// Feeds one foreground-process reading. Returns whether the label changed.
    pub fn observe(&mut self, name: &str) -> bool {
        if name == self.name {
            self.pending = None;
            return false;
        }

        let count = match self.pending.take() {
            Some((candidate, count)) if candidate == name => count + 1,
            _ => 1,
        };

        if count < CONFIRMATIONS {
            self.pending = Some((name.to_string(), count));
            return false;
        }

        let new_kind = AgentKind::detect(name);
        // A subprocess the agent spawned must not steal the tab's identity;
        // only the shell reappearing means the agent is really gone.
        if self.kind.is_agent() && !new_kind.is_agent() && new_kind != AgentKind::Shell {
            self.pending = None;
            return false;
        }

        self.name = name.to_string();
        self.kind = new_kind;
        self.pending = None;
        true
    }
}

/// Tracks output timing so `Activity` can be derived without polling the OS
/// for process state, which differs too much between platforms to trust.
pub struct ActivityTracker {
    last_output: Instant,
}

impl Default for ActivityTracker {
    fn default() -> Self {
        Self {
            last_output: Instant::now(),
        }
    }
}

impl ActivityTracker {
    pub fn mark_output(&mut self) {
        self.last_output = Instant::now();
    }

    pub fn activity(&self, kind: AgentKind) -> Activity {
        if self.last_output.elapsed() < QUIET_BEFORE_WAITING {
            Activity::Working
        } else if kind.is_agent() {
            Activity::Waiting
        } else {
            Activity::Idle
        }
    }
}

/// Name of the process currently in the foreground of the pty.
///
/// Returns `None` when the process can't be identified rather than guessing.
/// That happens routinely: a freshly spawned tab's foreground process is
/// `login`, which is setuid root, and `proc_name` refuses to report on it.
/// Callers should keep their previous value in that case.
pub fn foreground_process_name(master_fd: RawFd) -> Option<String> {
    let pgid = unsafe { libc::tcgetpgrp(master_fd) };
    let name = if pgid > 0 { process_name(pgid) } else { None };
    log::trace!("fd {master_fd}: pgid={pgid} name={name:?}");
    name
}

#[cfg(target_os = "macos")]
fn process_name(pid: i32) -> Option<String> {
    // `proc_name` lives in libproc, which is part of libSystem, so it needs no
    // extra link flags. libc doesn't declare it.
    extern "C" {
        fn proc_name(pid: i32, buffer: *mut libc::c_char, buffersize: u32) -> i32;
    }

    let mut buf = [0 as libc::c_char; 256];
    let len = unsafe { proc_name(pid, buf.as_mut_ptr(), buf.len() as u32) };
    if len <= 0 {
        return None;
    }
    let bytes: Vec<u8> = buf[..len as usize].iter().map(|c| *c as u8).collect();
    String::from_utf8(bytes).ok()
}

#[cfg(not(target_os = "macos"))]
fn process_name(pid: i32) -> Option<String> {
    // Linux and the other unixes with procfs.
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = comm.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_own_process_name() {
        let name = process_name(std::process::id() as i32).expect("no name for self");
        // The test harness binary is named after the crate.
        assert!(name.contains("shell"), "unexpected process name: {name:?}");
    }

    #[test]
    fn reads_child_process_name() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("5")
            .spawn()
            .expect("spawn sleep");
        let name = process_name(child.id() as i32);
        let _ = child.kill();
        assert_eq!(name.as_deref(), Some("sleep"));
    }

    /// Feeds a name enough times to clear the confirmation threshold.
    fn settle(tracker: &mut LabelTracker, name: &str) {
        for _ in 0..CONFIRMATIONS {
            tracker.observe(name);
        }
    }

    #[test]
    fn label_ignores_transient_subprocesses() {
        let mut tracker = LabelTracker::new("zsh".to_string());
        // A shell loop alternating between two short-lived processes: neither
        // ever survives two consecutive polls.
        for _ in 0..10 {
            assert!(!tracker.observe("sleep"));
            assert!(!tracker.observe("date"));
        }
        assert_eq!(tracker.name(), "zsh");
    }

    #[test]
    fn label_adopts_a_process_that_sticks_around() {
        let mut tracker = LabelTracker::new("zsh".to_string());
        assert!(!tracker.observe("vim"));
        assert!(tracker.observe("vim"));
        assert_eq!(tracker.name(), "vim");
        assert_eq!(tracker.kind(), AgentKind::Other);
    }

    #[test]
    fn agent_label_survives_tools_it_spawns() {
        let mut tracker = LabelTracker::new("zsh".to_string());
        settle(&mut tracker, "claude");
        assert_eq!(tracker.kind(), AgentKind::Claude);

        // The agent shells out; the tab must stay labeled "claude".
        for tool in ["git", "rg", "cargo", "node"] {
            settle(&mut tracker, tool);
        }
        assert_eq!(tracker.name(), "claude");
        assert_eq!(tracker.kind(), AgentKind::Claude);
    }

    #[test]
    fn agent_label_clears_when_shell_returns() {
        let mut tracker = LabelTracker::new("zsh".to_string());
        settle(&mut tracker, "claude");
        settle(&mut tracker, "zsh");
        assert_eq!(tracker.name(), "zsh");
        assert_eq!(tracker.kind(), AgentKind::Shell);
    }

    #[test]
    fn one_agent_can_replace_another() {
        let mut tracker = LabelTracker::new("zsh".to_string());
        settle(&mut tracker, "claude");
        settle(&mut tracker, "codex");
        assert_eq!(tracker.kind(), AgentKind::Codex);
    }

    #[test]
    fn classifies_agent_names() {
        assert_eq!(AgentKind::detect("claude"), AgentKind::Claude);
        assert_eq!(AgentKind::detect("codex"), AgentKind::Codex);
        assert_eq!(AgentKind::detect("zsh"), AgentKind::Shell);
        assert_eq!(AgentKind::detect("-zsh"), AgentKind::Shell);
        assert_eq!(AgentKind::detect("vim"), AgentKind::Other);
        // ssh must not be mistaken for a shell by a naive suffix check.
        assert_eq!(AgentKind::detect("ssh"), AgentKind::Other);
    }
}
