//! A single terminal session: the emulator state plus the pty feeding it.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Arc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, Notify, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop as PtyEventLoop, Msg, Notifier};
use alacritty_terminal::grid::Dimensions;
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
