//! Shell: a GPU-rendered, cross-platform terminal built around running
//! several coding agents at once.

mod agent;
mod clipboard;
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
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{ClearMode, Handler};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Theme as WindowTheme, Window, WindowId};

use clipboard::Clipboard;
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
/// A second click within this window, close enough to the first, escalates
/// the selection from character to word to line.
const MULTI_CLICK_WINDOW: Duration = Duration::from_millis(400);
/// How far a follow-up click may drift and still count as a multi-click.
const MULTI_CLICK_SLOP: f64 = 4.0;
/// Font size step for the zoom shortcuts, in logical pixels.
const FONT_STEP: f32 = 1.0;
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
    /// Set while the sidebar divider is being dragged.
    dragging_divider: bool,
    /// Set while a text selection is being dragged out in the grid.
    selecting: bool,
    last_click: Option<(Instant, PhysicalPosition<f64>)>,
    click_count: u32,
    clipboard: Clipboard,
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

    /// Grid point under the cursor, resolved through the active tab's
    /// scrollback offset.
    fn point_at(&self, position: PhysicalPosition<f64>) -> (Point, Side) {
        let session = self.active_session();
        let display_offset = session.term.lock().grid().display_offset();
        self.renderer.grid_point(
            position.x as f32,
            position.y as f32,
            session.size,
            display_offset,
        )
    }

    fn copy_selection(&mut self) {
        let text = self.active_session().term.lock().selection_to_string();
        if let Some(text) = text.filter(|t| !t.is_empty()) {
            self.clipboard.set(&text);
        }
    }

    fn paste(&mut self) {
        let Some(text) = self.clipboard.get() else {
            return;
        };
        let session = &self.sessions[self.active];
        paste_into(session, &text);
        self.renderer.window().request_redraw();
    }

    fn select_all(&mut self) {
        let session = &self.sessions[self.active];
        let mut term = session.term.lock();
        let mut selection =
            Selection::new(SelectionType::Simple, Point::new(Line(0), Column(0)), Side::Left);
        selection.include_all();
        term.selection = Some(selection);
        drop(term);
        self.renderer.window().request_redraw();
    }

    fn clear_scrollback(&mut self) {
        let session = &self.sessions[self.active];
        {
            let mut term = session.term.lock();
            term.clear_screen(ClearMode::All);
            term.clear_screen(ClearMode::Saved);
        }
        self.renderer.window().request_redraw();
    }

    fn adjust_font_size(&mut self, delta: f32) {
        let next = if delta == 0.0 {
            FONT_SIZE
        } else {
            self.renderer.font_size() + delta
        };
        self.renderer.set_font_size(next);
        self.resize_sessions();
        self.renderer.window().request_redraw();
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
            dragging_divider: false,
            selecting: false,
            last_click: None,
            click_count: 0,
            clipboard: Clipboard::new(),
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

            WindowEvent::CursorMoved { position, .. } => {
                state.cursor = position;

                if state.selecting {
                    let (point, side) = state.point_at(position);
                    if let Some(selection) =
                        state.sessions[state.active].term.lock().selection.as_mut()
                    {
                        selection.update(point, side);
                    }
                    state.renderer.window().request_redraw();
                    return;
                }

                if state.dragging_divider {
                    let logical = position.x as f32 / state.renderer.scale;
                    if state.renderer.set_sidebar_width(logical) {
                        // The grid narrowed or widened, so the ptys have to be
                        // told about their new size.
                        state.resize_sessions();
                        state.renderer.window().request_redraw();
                    }
                    return;
                }

                let over_divider = matches!(
                    state.renderer.layout.hit_test(
                        position.x as f32,
                        position.y as f32,
                        state.sessions.len()
                    ),
                    Hit::Divider
                );
                state.renderer.window().set_cursor(if over_divider {
                    CursorIcon::ColResize
                } else {
                    CursorIcon::Default
                });
            },

            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                state.dragging_divider = false;
                state.selecting = false;
            },

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
                    Hit::Divider => state.dragging_divider = true,
                    Hit::Grid => {
                        let now = Instant::now();
                        let is_multi = state.last_click.is_some_and(|(at, pos)| {
                            now.duration_since(at) < MULTI_CLICK_WINDOW
                                && (pos.x - state.cursor.x).abs() < MULTI_CLICK_SLOP
                                && (pos.y - state.cursor.y).abs() < MULTI_CLICK_SLOP
                        });
                        state.click_count = if is_multi {
                            state.click_count % 3 + 1
                        } else {
                            1
                        };
                        state.last_click = Some((now, state.cursor));

                        let ty = match state.click_count {
                            2 => SelectionType::Semantic,
                            3 => SelectionType::Lines,
                            _ => SelectionType::Simple,
                        };
                        let (point, side) = state.point_at(state.cursor);
                        state.sessions[state.active].term.lock().selection =
                            Some(Selection::new(ty, point, side));
                        state.selecting = true;
                        state.renderer.window().request_redraw();
                    },
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
                            "c" => {
                                state.copy_selection();
                                return;
                            },
                            "v" => {
                                state.paste();
                                return;
                            },
                            "a" => {
                                state.select_all();
                                return;
                            },
                            "k" => {
                                state.clear_scrollback();
                                return;
                            },
                            "+" | "=" => {
                                state.adjust_font_size(FONT_STEP);
                                return;
                            },
                            "-" => {
                                state.adjust_font_size(-FONT_STEP);
                                return;
                            },
                            "0" => {
                                state.adjust_font_size(0.0);
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
                            digit
                                if digit.len() == 1
                                    && digit.chars().all(|c| c.is_ascii_digit()) =>
                            {
                                if let Ok(n) = digit.parse::<usize>() {
                                    if n >= 1 {
                                        state.activate(n - 1);
                                    }
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
                    {
                        // Typing dismisses the selection and snaps the
                        // viewport back to the prompt.
                        let mut term = session.term.lock();
                        term.selection = None;
                        term.scroll_display(Scroll::Bottom);
                    }
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
                // Activity is derived from grid content during polling, not
                // from output arriving; this only drives the repaint.
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

            // OSC 52: a program asking to read or write the system clipboard.
            TermEvent::ClipboardStore(_, text) => state.clipboard.set(&text),
            TermEvent::ClipboardLoad(_, format) => {
                if let Some(text) = state.clipboard.get() {
                    let reply = format(&text);
                    state.sessions[index].write(reply.into_bytes());
                }
            },

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

/// Writes pasted text to a pty.
///
/// Newlines are normalized to carriage returns so a multi-line paste behaves
/// like typing Return. Under bracketed paste the payload is stripped of escape
/// characters, so pasted content can't close the bracket early and be executed
/// as if typed.
fn paste_into(session: &Session, text: &str) {
    let bracketed = session
        .term
        .lock()
        .mode()
        .contains(TermMode::BRACKETED_PASTE);

    let mut body = text.replace("\r\n", "\r").replace('\n', "\r");

    if bracketed {
        body.retain(|c| c != '\u{1b}');
        session.write(b"\x1b[200~".to_vec());
        session.write(body.into_bytes());
        session.write(b"\x1b[201~".to_vec());
    } else {
        session.write(body.into_bytes());
    }
}
