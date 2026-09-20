//! Shell: a GPU-rendered, cross-platform terminal built around running
//! several coding agents at once.

mod agent;
mod clipboard;
mod find;
mod input;
mod links;
mod palette;
mod quad;
mod renderer;
mod session;
mod theme;
mod themes;
mod ui;

use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event as TermEvent, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{ClearMode, Handler};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{CursorIcon, Theme as WindowTheme, Window, WindowId};

use clipboard::Clipboard;
use palette::{PaletteAction, PaletteMode, PaletteState};
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
    /// Link under the cursor while the app modifier is held.
    hovered_link: Option<(Line, std::ops::Range<usize>, String)>,
    click_count: u32,
    clipboard: Clipboard,
    find: Option<find::FindState>,
    palette: Option<PaletteState>,
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

    /// Copy-on-select, for the primary selection only. Following the X11
    /// convention here does not disturb the ordinary clipboard.
    fn copy_selection_to_primary(&mut self) {
        let text = self.active_session().term.lock().selection_to_string();
        if let Some(text) = text.filter(|t| !t.is_empty()) {
            self.clipboard.set_primary(&text);
        }
    }

    fn paste_primary(&mut self) {
        let Some(text) = self.clipboard.get_primary() else {
            return;
        };
        paste_into(&self.sessions[self.active], &text);
        self.renderer.window().request_redraw();
    }

    /// Link under the cursor, resolved against the row it is over.
    fn link_at(
        &self,
        position: PhysicalPosition<f64>,
    ) -> Option<(Line, std::ops::Range<usize>, String)> {
        let (point, _) = self.point_at(position);
        let text = self.active_session().row_text(point.line);
        let (range, url) = links::find_url_at(&text, point.column.0)?;
        Some((point.line, range, url))
    }

    /// Recomputes the hovered link. Links only light up while the app
    /// modifier is held, so ordinary dragging still selects text over them.
    fn update_hovered_link(&mut self, position: PhysicalPosition<f64>) -> bool {
        let next = if is_app_modifier(self.mods) {
            self.link_at(position)
        } else {
            None
        };
        let key = |l: &Option<(Line, std::ops::Range<usize>, String)>| {
            l.as_ref().map(|(line, range, _)| (*line, range.clone()))
        };
        if key(&next) == key(&self.hovered_link) {
            return false;
        }
        self.renderer
            .set_link(key(&next));
        self.hovered_link = next;
        true
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
        if let Some(ref mut find) = self.find {
            let term = self.sessions[index].term.lock();
            find.search(&term);
        }
        self.renderer.window().request_redraw();
    }

    fn scroll_to_find_match(&mut self) {
        let Some(ref find) = self.find else {
            return;
        };
        let Some(m) = find.current_match() else {
            return;
        };
        let line = m.line;
        let mut term = self.sessions[self.active].term.lock();
        let display_offset = term.grid().display_offset() as i32;
        let screen_lines = term.grid().screen_lines() as i32;
        let viewport_line = line.0 + display_offset;
        if viewport_line < 0 || viewport_line >= screen_lines {
            let target = screen_lines / 3;
            let new_offset = target - line.0;
            let delta = new_offset - display_offset;
            term.scroll_display(Scroll::Delta(delta));
        }
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
            hovered_link: None,
            click_count: 0,
            clipboard: Clipboard::new(),
            find: None,
            palette: None,
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

            WindowEvent::ModifiersChanged(mods) => {
                state.mods = mods.state();
                let cursor = state.cursor;
                if state.update_hovered_link(cursor) {
                    state.renderer.window().request_redraw();
                }
            },

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

                if state.update_hovered_link(position) {
                    state.renderer.window().request_redraw();
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
                } else if state.hovered_link.is_some() {
                    CursorIcon::Pointer
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
                if state.selecting {
                    state.selecting = false;
                    state.copy_selection_to_primary();
                    state.copy_selection();
                }
            },

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Middle,
                ..
            } => state.paste_primary(),

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                let has_selection = state.active_session().term.lock().selection.is_some();
                if has_selection {
                    state.copy_selection();
                } else {
                    state.paste();
                }
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
                        if let Some((_, _, url)) = state.hovered_link.clone() {
                            links::open(&url);
                            return;
                        }

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
                let is_cmd = state.mods.super_key();
                let is_ctrl_shift = state.mods.control_key() && state.mods.shift_key();
                let is_app_shortcut = is_cmd || is_ctrl_shift;

                if is_app_shortcut {
                    let matches_key = |target_code: KeyCode, target_ch: char| -> bool {
                        if matches!(event.physical_key, PhysicalKey::Code(code) if code == target_code) {
                            return true;
                        }
                        if let Key::Character(ref s) = event.logical_key {
                            if s.eq_ignore_ascii_case(&target_ch.to_string()) {
                                return true;
                            }
                        }
                        false
                    };

                    if matches_key(KeyCode::KeyC, 'c') {
                        let selected = state.active_session().term.lock().selection_to_string();
                        if let Some(text) = selected.filter(|t| !t.is_empty()) {
                            state.clipboard.set(&text);
                        } else {
                            // No selection: send SIGINT (\x03) so Cmd+C cancels running process / prompt
                            state.sessions[state.active].write(vec![0x03]);
                        }
                        state.renderer.window().request_redraw();
                        return;
                    }

                    if matches_key(KeyCode::KeyX, 'x') {
                        let selected = state.active_session().term.lock().selection_to_string();
                        if let Some(text) = selected.filter(|t| !t.is_empty()) {
                            state.clipboard.set(&text);
                            state.sessions[state.active].term.lock().selection = None;
                        } else {
                            // Cut current line at prompt: copy row text and send Ctrl+U (\x15)
                            let line_text = {
                                let session = state.active_session();
                                let term = session.term.lock();
                                let cursor = term.grid().cursor.point;
                                session.row_text(cursor.line)
                            };
                            let trimmed = line_text.trim_end();
                            if !trimmed.is_empty() {
                                state.clipboard.set(trimmed);
                            }
                            state.sessions[state.active].write(vec![0x15]);
                        }
                        state.renderer.window().request_redraw();
                        return;
                    }

                    if matches_key(KeyCode::KeyV, 'v') {
                        if let Some(ref mut find) = state.find {
                            if let Some(text) = state.clipboard.get() {
                                let first_line = text.lines().next().unwrap_or("");
                                find.insert_str(first_line);
                                let term = state.sessions[state.active].term.lock();
                                find.search(&term);
                            }
                            state.renderer.window().request_redraw();
                        } else if let Some(ref mut palette) = state.palette {
                            if let Some(text) = state.clipboard.get() {
                                let first_line = text.lines().next().unwrap_or("");
                                for ch in first_line.chars() {
                                    if !ch.is_control() {
                                        palette.insert_char(ch);
                                    }
                                }
                                palette.update_items(&state.sessions, state.active);
                            }
                            state.renderer.window().request_redraw();
                        } else {
                            state.paste();
                        }
                        return;
                    }

                    if matches_key(KeyCode::KeyA, 'a') {
                        state.select_all();
                        return;
                    }

                    if matches_key(KeyCode::KeyT, 't') {
                        match spawn_session(
                            &mut self.next_id,
                            &self.proxy,
                            &state.renderer,
                        ) {
                            Ok(session) => state.push_session(session),
                            Err(err) => log::error!("failed to spawn pty: {err}"),
                        }
                        return;
                    }

                    if matches_key(KeyCode::KeyW, 'w') {
                        let active = state.active;
                        if !state.close(active) {
                            event_loop.exit();
                        }
                        return;
                    }

                    if matches_key(KeyCode::KeyK, 'k') {
                        state.clear_scrollback();
                        return;
                    }

                    if matches!(event.physical_key, PhysicalKey::Code(KeyCode::Equal | KeyCode::NumpadAdd))
                        || matches!(&event.logical_key, Key::Character(c) if c.as_str() == "+" || c.as_str() == "=")
                    {
                        state.adjust_font_size(FONT_STEP);
                        return;
                    }

                    if matches!(event.physical_key, PhysicalKey::Code(KeyCode::Minus | KeyCode::NumpadSubtract))
                        || matches!(&event.logical_key, Key::Character(c) if c.as_str() == "-")
                    {
                        state.adjust_font_size(-FONT_STEP);
                        return;
                    }

                    if matches!(event.physical_key, PhysicalKey::Code(KeyCode::Digit0 | KeyCode::Numpad0))
                        || matches!(&event.logical_key, Key::Character(c) if c.as_str() == "0")
                    {
                        state.adjust_font_size(0.0);
                        return;
                    }

                    if matches_key(KeyCode::BracketRight, ']') {
                        state.cycle(true);
                        return;
                    }

                    if matches_key(KeyCode::BracketLeft, '[') {
                        state.cycle(false);
                        return;
                    }

                    if matches_key(KeyCode::KeyF, 'f') {
                        let initial = {
                            let term = state.sessions[state.active].term.lock();
                            term.selection_to_string().unwrap_or_default()
                        };
                        if initial.is_empty() {
                            state.find.get_or_insert_with(find::FindState::new);
                        } else {
                            state.find = Some(find::FindState::with_query(&initial));
                        }
                        if let Some(ref mut find) = state.find {
                            let term = state.sessions[state.active].term.lock();
                            find.search(&term);
                        }
                        state.renderer.window().request_redraw();
                        return;
                    }

                    if matches_key(KeyCode::KeyG, 'g') {
                        if let Some(ref mut find) = state.find {
                            if state.mods.shift_key() {
                                find.prev_match();
                            } else {
                                find.next_match();
                            }
                            state.scroll_to_find_match();
                            state.renderer.window().request_redraw();
                        }
                        return;
                    }

                    if matches_key(KeyCode::KeyQ, 'q') {
                        for session in &state.sessions {
                            session.shutdown();
                        }
                        event_loop.exit();
                        return;
                    }

                    if matches_key(KeyCode::KeyP, 'p') {
                        if state
                            .palette
                            .as_ref()
                            .map(|p| p.mode == PaletteMode::Commands)
                            .unwrap_or(false)
                        {
                            state.palette = None;
                        } else {
                            let mut p = PaletteState::new(PaletteMode::Commands);
                            p.update_items(&state.sessions, state.active);
                            state.palette = Some(p);
                        }
                        state.renderer.window().request_redraw();
                        return;
                    }

                    if matches_key(KeyCode::KeyR, 'r') {
                        if state
                            .palette
                            .as_ref()
                            .map(|p| p.mode == PaletteMode::History)
                            .unwrap_or(false)
                        {
                            state.palette = None;
                        } else {
                            let mut p = PaletteState::new(PaletteMode::History);
                            p.update_items(&state.sessions, state.active);
                            state.palette = Some(p);
                        }
                        state.renderer.window().request_redraw();
                        return;
                    }

                    if matches!(&event.logical_key, Key::Named(NamedKey::Tab)) {
                        state.cycle(!state.mods.shift_key());
                        return;
                    }

                    let digit_opt = match &event.physical_key {
                        PhysicalKey::Code(KeyCode::Digit1) => Some(1),
                        PhysicalKey::Code(KeyCode::Digit2) => Some(2),
                        PhysicalKey::Code(KeyCode::Digit3) => Some(3),
                        PhysicalKey::Code(KeyCode::Digit4) => Some(4),
                        PhysicalKey::Code(KeyCode::Digit5) => Some(5),
                        PhysicalKey::Code(KeyCode::Digit6) => Some(6),
                        PhysicalKey::Code(KeyCode::Digit7) => Some(7),
                        PhysicalKey::Code(KeyCode::Digit8) => Some(8),
                        PhysicalKey::Code(KeyCode::Digit9) => Some(9),
                        _ => {
                            if let Key::Character(ref c) = event.logical_key {
                                c.parse::<usize>().ok()
                            } else {
                                None
                            }
                        },
                    };
                    if let Some(n) = digit_opt {
                        if n >= 1 {
                            state.activate(n - 1);
                        }
                        return;
                    }
                }

                // Palette mode (Arc/Dia-style visual fuzzy command & history overlay):
                // keys go to the palette, not the pty.
                if state.palette.is_some() {
                    let mut close_palette = false;
                    let mut action_to_execute = None;
                    let mut insert_to_pty = None;

                    {
                        let palette = state.palette.as_mut().unwrap();
                        match &event.logical_key {
                            Key::Named(NamedKey::Escape) => {
                                close_palette = true;
                            },
                            Key::Named(NamedKey::ArrowUp) => {
                                palette.select_prev();
                            },
                            Key::Named(NamedKey::ArrowDown) => {
                                palette.select_next();
                            },
                            Key::Named(NamedKey::Enter) => {
                                if let Some(item) = palette.current_item() {
                                    action_to_execute = Some(item.action.clone());
                                }
                                close_palette = true;
                            },
                            Key::Named(NamedKey::Tab) => {
                                if let Some(item) = palette.current_item() {
                                    if let PaletteAction::WriteToPty { ref text, .. } = item.action {
                                        insert_to_pty = Some(text.clone());
                                    }
                                }
                                close_palette = true;
                            },
                            Key::Named(NamedKey::Backspace) => {
                                if state.mods.alt_key() {
                                    palette.delete_word();
                                } else {
                                    palette.backspace();
                                }
                                palette.update_items(&state.sessions, state.active);
                            },
                            Key::Named(NamedKey::ArrowLeft) => palette.cursor_left(),
                            Key::Named(NamedKey::ArrowRight) => palette.cursor_right(),
                            Key::Named(NamedKey::Home) => palette.cursor_home(),
                            Key::Named(NamedKey::End) => palette.cursor_end(),
                            Key::Character(c) => {
                                if state.mods.control_key() {
                                    match c.as_str() {
                                        "p" => palette.select_prev(),
                                        "n" => palette.select_next(),
                                        "u" => {
                                            palette.query.clear();
                                            palette.cursor_pos = 0;
                                            palette.update_items(&state.sessions, state.active);
                                        },
                                        "w" => {
                                            palette.delete_word();
                                            palette.update_items(&state.sessions, state.active);
                                        },
                                        _ => {},
                                    }
                                } else if !state.mods.super_key() {
                                    let input = event.text.as_deref().unwrap_or(c.as_str());
                                    for ch in input.chars() {
                                        if !ch.is_control() {
                                            palette.insert_char(ch);
                                        }
                                    }
                                    palette.update_items(&state.sessions, state.active);
                                }
                            },
                            _ => {},
                        }
                    }

                    if close_palette {
                        state.palette = None;
                    }

                    if let Some(text) = insert_to_pty {
                        let session = &state.sessions[state.active];
                        session.write(text.into_bytes());
                    }

                    if let Some(action) = action_to_execute {
                        match action {
                            PaletteAction::WriteToPty { text, execute } => {
                                let session = &state.sessions[state.active];
                                let mut bytes = text.into_bytes();
                                if execute {
                                    bytes.push(b'\r');
                                }
                                session.write(bytes);
                            },
                            PaletteAction::SwitchTab(index) => {
                                state.activate(index);
                            },
                            PaletteAction::SpawnAgent { name: _, command } => {
                                match spawn_session(&mut self.next_id, &self.proxy, &state.renderer) {
                                    Ok(session) => {
                                        if let Some(cmd) = command {
                                            session.write(format!("{cmd}\r").into_bytes());
                                        }
                                        state.push_session(session);
                                    },
                                    Err(err) => log::error!("failed to spawn pty: {err}"),
                                }
                            },
                            PaletteAction::ClearScrollback => {
                                state.clear_scrollback();
                            },
                            PaletteAction::FindInBuffer => {
                                state.find.get_or_insert_with(find::FindState::new);
                            },
                            PaletteAction::Zoom(delta) => {
                                state.adjust_font_size(delta * FONT_STEP);
                            },
                        }
                    }

                    state.renderer.window().request_redraw();
                    return;
                }

                // Find mode: keys go to the search bar, not the pty.
                if state.find.is_some() {
                    let mut close_find = false;
                    {
                        let find = state.find.as_mut().unwrap();
                        match &event.logical_key {
                            Key::Named(NamedKey::Escape) => {
                                close_find = true;
                            },
                            Key::Named(NamedKey::Enter) => {
                                if state.mods.shift_key() {
                                    find.prev_match();
                                } else {
                                    find.next_match();
                                }
                            },
                            Key::Named(NamedKey::Backspace) => {
                                if state.mods.alt_key() {
                                    find.delete_word();
                                } else {
                                    find.backspace();
                                }
                                let term = state.sessions[state.active].term.lock();
                                find.search(&term);
                            },
                            Key::Named(NamedKey::Delete) => {
                                find.delete_forward();
                                let term = state.sessions[state.active].term.lock();
                                find.search(&term);
                            },
                            Key::Named(NamedKey::ArrowLeft) => find.cursor_left(),
                            Key::Named(NamedKey::ArrowRight) => find.cursor_right(),
                            Key::Named(NamedKey::Home) => find.cursor_home(),
                            Key::Named(NamedKey::End) => find.cursor_end(),
                            Key::Character(c) => {
                                if state.mods.control_key() {
                                    match c.as_str() {
                                        "a" => find.cursor_home(),
                                        "e" => find.cursor_end(),
                                        "k" => find.kill_to_end(),
                                        "u" => find.kill_to_start(),
                                        "w" => find.delete_word(),
                                        _ => {},
                                    }
                                    let term = state.sessions[state.active].term.lock();
                                    find.search(&term);
                                } else {
                                    let input = event.text.as_deref().unwrap_or(c.as_str());
                                    for ch in input.chars() {
                                        if !ch.is_control() {
                                            find.insert(ch);
                                        }
                                    }
                                    let term = state.sessions[state.active].term.lock();
                                    find.search(&term);
                                }
                            },
                            _ => {},
                        }
                    }
                    if close_find {
                        state.find = None;
                    } else {
                        state.scroll_to_find_match();
                    }
                    state.renderer.window().request_redraw();
                    return;
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
                state.renderer.render(
                    &state.sessions,
                    active,
                    breathe,
                    state.find.as_ref(),
                    state.palette.as_ref(),
                );
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
