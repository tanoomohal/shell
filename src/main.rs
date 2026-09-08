//! Shell: a GPU-rendered, cross-platform terminal built around running
//! several coding agents at once.

mod agent;
mod input;
mod quad;
mod renderer;
mod session;
mod theme;
mod themes;
mod ui;

use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event as TermEvent, WindowSize};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::tty;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Theme as WindowTheme, Window, WindowId};

use renderer::Renderer;
use session::{Session, TermSize, UserEvent};
use ui::Hit;

pub const APP_NAME: &str = "Shell";
const FONT_FAMILY: &str = "Menlo";
const FONT_SIZE: f32 = 13.0;
/// Lines scrolled per notch of a discrete mouse wheel.
const LINES_PER_WHEEL_NOTCH: f64 = 3.0;
/// How often the pty foreground process and activity state are re-read.
const POLL_INTERVAL: Duration = Duration::from_millis(400);
/// Full cycle of the attention pulse, in seconds.
const BREATHE_PERIOD: f32 = 1.6;
/// Frame interval while the attention pulse is running. Only ticks this fast
/// while at least one tab is unattended; idle costs zero frames.
const ANIMATION_INTERVAL: Duration = Duration::from_millis(33);

fn main() {
    env_logger::init();

    // Advertise the right TERM and truecolor support to child processes.
    tty::setup_env();

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("failed to create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let proxy = event_loop.create_proxy();
    let mut app = App {
        proxy,
        state: None,
        next_id: 0,
    };
    event_loop.run_app(&mut app).expect("event loop failed");
}

struct State {
    sessions: Vec<Session>,
    active: usize,
    mods: ModifiersState,
    cursor: PhysicalPosition<f64>,
    /// Fractional wheel remainder, so trackpad scrolling doesn't lose motion.
    scroll_remainder: f64,
    /// Clock for the attention pulse.
    started: Instant,
    renderer: Renderer,
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    state: Option<State>,
    next_id: u64,
}

/// The platform's "application shortcut" modifier: Command on macOS, and
/// Ctrl+Shift elsewhere, since bare Ctrl belongs to the terminal.
fn is_app_modifier(mods: ModifiersState) -> bool {
    #[cfg(target_os = "macos")]
    {
        mods.super_key()
    }
    #[cfg(not(target_os = "macos"))]
    {
        mods.control_key() && mods.shift_key()
    }
}

fn window_size(renderer: &Renderer, grid: TermSize) -> WindowSize {
    WindowSize {
        num_lines: grid.screen_lines as u16,
        num_cols: grid.columns as u16,
        cell_width: renderer.metrics.width as u16,
        cell_height: renderer.metrics.height as u16,
    }
}

/// Free function rather than a method on `App` so it can be called while
/// `App::state` is mutably borrowed.
fn spawn_session(
    next_id: &mut u64,
    proxy: &EventLoopProxy<UserEvent>,
    renderer: &Renderer,
) -> std::io::Result<Session> {
    let grid = renderer.grid_size();
    let id = *next_id;
    *next_id += 1;
    Session::new(id, grid, window_size(renderer, grid), proxy)
}

impl State {
    fn wants_attention(&self) -> bool {
        self.sessions.iter().any(|s| s.needs_attention)
    }

    /// 0..1 sine ramp driving the attention pulse. Held at 1.0 when nothing
    /// is waiting, so no tab shimmers for no reason.
    fn breathe(&self) -> f32 {
        if !self.wants_attention() {
            return 1.0;
        }
        let t = self.started.elapsed().as_secs_f32();
        0.5 - 0.5 * (t * std::f32::consts::TAU / BREATHE_PERIOD).cos()
    }

    fn active_session(&self) -> &Session {
        &self.sessions[self.active]
    }

    fn resize_sessions(&mut self) {
        let grid = self.renderer.grid_size();
        let size = window_size(&self.renderer, grid);
        for session in &mut self.sessions {
            session.resize(grid, size);
        }
    }

    fn push_session(&mut self, session: Session) {
        self.sessions.push(session);
        self.active = self.sessions.len() - 1;
        self.renderer.window().request_redraw();
    }

    fn activate(&mut self, index: usize) {
        if index >= self.sessions.len() || index == self.active {
            return;
        }
        self.active = index;
        self.sessions[index].needs_attention = false;
        self.renderer.window().request_redraw();
    }

    fn cycle(&mut self, forward: bool) {
        let count = self.sessions.len();
        if count < 2 {
            return;
        }
        let next = if forward {
            (self.active + 1) % count
        } else {
            (self.active + count - 1) % count
        };
        self.activate(next);
    }

    /// Closes a tab, returning false when that was the last one and the app
    /// should exit.
    fn close(&mut self, index: usize) -> bool {
        if index >= self.sessions.len() {
            return true;
        }
        self.sessions[index].shutdown();
        self.sessions.remove(index);

        if self.sessions.is_empty() {
            return false;
        }
        if self.active >= self.sessions.len() {
            self.active = self.sessions.len() - 1;
        } else if index < self.active {
            self.active -= 1;
        }
        self.renderer.window().request_redraw();
        true
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title(APP_NAME)
            .with_inner_size(LogicalSize::new(1080.0, 660.0))
            .with_min_inner_size(LogicalSize::new(520.0, 260.0));

        // Native chrome, but with the content view running the full height so
        // the sidebar reaches the top behind the traffic lights. This is the
        // source-list idiom (Xcode, Mail, Arc), not a custom window frame.
        #[cfg(target_os = "macos")]
        let attributes = {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attributes
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true)
                .with_title_hidden(true)
        };
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .expect("failed to create window"),
        );

        let mut renderer = pollster::block_on(Renderer::new(
            window,
            event_loop,
            FONT_FAMILY.to_string(),
            FONT_SIZE,
        ));
        // Match system appearance at launch; `ThemeChanged` keeps it in sync.
        let dark = renderer.window().theme() != Some(WindowTheme::Light);
        renderer.set_appearance(dark);

        let session = spawn_session(&mut self.next_id, &self.proxy, &renderer)
            .expect("failed to spawn pty");

        renderer.window().request_redraw();
        self.state = Some(State {
            sessions: vec![session],
            active: 0,
            mods: ModifiersState::empty(),
            cursor: PhysicalPosition::new(0.0, 0.0),
            scroll_remainder: 0.0,
            started: Instant::now(),
            renderer,
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = &mut self.state else {
            return;
        };

        match event {
            WindowEvent::CloseRequested => {
                for session in &state.sessions {
                    session.shutdown();
                }
                event_loop.exit();
            },

            WindowEvent::Resized(size) => {
                state.renderer.resize(size.width, size.height);
                state.resize_sessions();
                state.renderer.window().request_redraw();
            },

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                state.renderer.set_scale(scale_factor as f32);
                state.resize_sessions();
                state.renderer.window().request_redraw();
            },

            WindowEvent::ThemeChanged(theme) => {
                state.renderer.set_appearance(theme == WindowTheme::Dark);
                state.renderer.window().request_redraw();
            },

            WindowEvent::ModifiersChanged(mods) => state.mods = mods.state(),

            WindowEvent::CursorMoved { position, .. } => state.cursor = position,

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let hit = state.renderer.layout.hit_test(
                    state.cursor.x as f32,
                    state.cursor.y as f32,
                    state.sessions.len(),
                );
                match hit {
                    Hit::NewTab => {
                        match spawn_session(&mut self.next_id, &self.proxy, &state.renderer) {
                            Ok(session) => state.push_session(session),
                            Err(err) => log::error!("failed to spawn pty: {err}"),
                        }
                    },
                    Hit::Tab(index) => state.activate(index),
                    Hit::Close(index) => {
                        if !state.close(index) {
                            event_loop.exit();
                        }
                    },
                    Hit::Grid => {},
                }
            },

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }

                // Application shortcuts are handled before the pty sees the key.
                if is_app_modifier(state.mods) {
                    match &event.logical_key {
                        Key::Character(c) => match c.as_str() {
                            "t" => {
                                match spawn_session(
                                    &mut self.next_id,
                                    &self.proxy,
                                    &state.renderer,
                                ) {
                                    Ok(session) => state.push_session(session),
                                    Err(err) => log::error!("failed to spawn pty: {err}"),
                                }
                                return;
                            },
                            "w" => {
                                let active = state.active;
                                if !state.close(active) {
                                    event_loop.exit();
                                }
                                return;
                            },
                            "]" => {
                                state.cycle(true);
                                return;
                            },
                            "[" => {
                                state.cycle(false);
                                return;
                            },
                            digit if digit.len() == 1 && digit.chars().all(|c| c.is_ascii_digit()) => {
                                let n: usize = digit.parse().unwrap_or(0);
                                if n >= 1 {
                                    state.activate(n - 1);
                                }
                                return;
                            },
                            _ => {},
                        },
                        Key::Named(NamedKey::Tab) => {
                            state.cycle(!state.mods.shift_key());
                            return;
                        },
                        _ => {},
                    }
                }

                let app_cursor = state
                    .active_session()
                    .term
                    .lock()
                    .mode()
                    .contains(TermMode::APP_CURSOR);

                if let Some(bytes) = input::encode(
                    &event.logical_key,
                    event.text.as_deref(),
                    state.mods,
                    app_cursor,
                ) {
                    let session = &state.sessions[state.active];
                    // Typing always snaps the viewport back to the prompt.
                    session.term.lock().scroll_display(Scroll::Bottom);
                    session.write(bytes);
                    state.renderer.window().request_redraw();
                }
            },

            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64 * LINES_PER_WHEEL_NOTCH,
                    MouseScrollDelta::PixelDelta(pos) => {
                        pos.y / state.renderer.metrics.height as f64
                    },
                };

                state.scroll_remainder += lines;
                let whole = state.scroll_remainder.trunc();
                state.scroll_remainder -= whole;

                if whole != 0.0 {
                    state
                        .active_session()
                        .term
                        .lock()
                        .scroll_display(Scroll::Delta(whole as i32));
                    state.renderer.window().request_redraw();
                }
            },

            WindowEvent::RedrawRequested => {
                let active = state.active;
                let breathe = state.breathe();
                state.renderer.render(&state.sessions, active, breathe);
            },

            _ => {},
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let Some(state) = &mut self.state else {
            return;
        };
        let Some(index) = state.sessions.iter().position(|s| s.id == event.session) else {
            return;
        };

        match event.event {
            TermEvent::Wakeup => {
                // Output arriving is what distinguishes "working" from
                // "waiting on you".
                state.sessions[index].mark_output();
                if index == state.active {
                    state.renderer.window().request_redraw();
                }
            },

            TermEvent::Title(title) => state.sessions[index].title = title,
            TermEvent::ResetTitle => state.sessions[index].title.clear(),

            TermEvent::Bell => {
                if index != state.active {
                    state.sessions[index].flag_attention();
                    state.renderer.window().request_redraw();
                }
            },

            TermEvent::PtyWrite(text) => state.sessions[index].write(text.into_bytes()),

            TermEvent::Exit | TermEvent::ChildExit(_) => {
                if !state.close(index) {
                    event_loop.exit();
                }
            },

            // Clipboard and cursor blinking land in a later milestone.
            _ => {},
        }
    }

    /// Polls each pty's foreground process so tab labels and activity dots
    /// stay current without the pty having to tell us anything.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(state) = &mut self.state else {
            return;
        };

        let active = state.active;
        let mut changed = false;
        for (index, session) in state.sessions.iter_mut().enumerate() {
            changed |= session.poll_state(index == active);
        }
        if changed {
            state.renderer.window().request_redraw();
        }

        // Animate only while something is actually waiting on the user.
        let interval = if state.wants_attention() {
            state.renderer.window().request_redraw();
            ANIMATION_INTERVAL
        } else {
            POLL_INTERVAL
        };
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + interval));
    }
}
