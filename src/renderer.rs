//! wgpu renderer: cell backgrounds and window chrome as instanced quads,
//! glyphs via glyphon.

use std::sync::Arc;

use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{point_to_viewport, viewport_to_point};
use alacritty_terminal::vte::ansi::CursorShape;
use glyphon::cosmic_text::{Ellipsize, EllipsizeHeightLimit};
use glyphon::{
    Attrs, Buffer as TextBuffer, Cache, Color as TextColor, Family, FontSystem, Metrics,
    Resolution, Shaping, Style, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer,
    Viewport, Weight, Wrap,
};
use wgpu::{
    CommandEncoderDescriptor, CompositeAlphaMode, CurrentSurfaceTexture, Device, DeviceDescriptor,
    Instance, InstanceDescriptor, LoadOp, MultisampleState, Operations, PresentMode, Queue,
    RenderPassColorAttachment, RenderPassDescriptor, RequestAdapterOptions, Surface,
    SurfaceColorSpace, SurfaceConfiguration, TextureFormat, TextureUsages, TextureViewDescriptor,
};
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::agent::Activity;
use crate::find::FindState;
use crate::palette::{PaletteCategory, PaletteMode, PaletteState};
use crate::quad::{Quad, QuadPipeline};
use crate::session::{Session, TermSize};
use crate::theme::{linear_rgba, mix, Rgb8, Theme};
use crate::themes;
use crate::ui::{self, Layout};

/// Inset between the grid and its container, in logical pixels.
pub const PADDING: f32 = 8.0;
pub const MIN_FONT_SIZE: f32 = 6.0;
pub const MAX_FONT_SIZE: f32 = 48.0;
/// Minimum WCAG contrast enforced between cell text and its background.
/// Agent CLIs lean on dim grays chosen against some other background, so a
/// floor here is what keeps them readable under an arbitrary theme.
const MIN_CONTRAST: f32 = 1.3;
/// Chrome text size, in logical pixels.
const CHROME_FONT_SIZE: f32 = 12.5;
const CHROME_LINE_HEIGHT: f32 = 16.0;

const SURFACE_FORMAT: TextureFormat = TextureFormat::Bgra8UnormSrgb;

#[derive(Clone, Copy, Debug)]
pub struct CellMetrics {
    /// Advance width of one cell in physical pixels.
    pub width: f32,
    /// Line height in physical pixels.
    pub height: f32,
    pub font_size: f32,
}

/// One run of same-styled text in the frame's grid string.
struct Span {
    start: usize,
    end: usize,
    color: Rgb8,
    flags: Flags,
}

/// A single line of chrome text, positioned in physical pixels. Font size and
/// line height are logical, scaled when the buffer is shaped.
struct Label {
    text: String,
    x: f32,
    y: f32,
    max_width: f32,
    color: Rgb8,
    font_size: f32,
    line_height: f32,
}

pub struct Renderer {
    instance: Instance,
    device: Device,
    queue: Queue,
    surface: Surface<'static>,
    surface_config: SurfaceConfiguration,

    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    modal_text_renderer: TextRenderer,

    grid_buf: TextBuffer,
    chrome_bufs: Vec<TextBuffer>,
    labels: Vec<Label>,

    modal_bufs: Vec<TextBuffer>,
    modal_labels: Vec<Label>,

    /// Grid line and column range of the link under the cursor, if any.
    link: Option<(Line, std::ops::Range<usize>)>,

    quads: QuadPipeline,
    under: Vec<Quad>,
    over: Vec<Quad>,
    modal_under: Vec<Quad>,
    modal_over: Vec<Quad>,
    text: String,
    spans: Vec<Span>,

    font_family: String,
    base_font_size: f32,
    /// User-resizable sidebar width, in logical pixels.
    sidebar_logical: f32,
    pub metrics: CellMetrics,
    pub theme: Theme,
    pub layout: Layout,
    pub scale: f32,

    // Dropped after `surface`, which borrows the window handle.
    window: Arc<Window>,
}

impl Renderer {
    pub async fn new(
        window: Arc<Window>,
        event_loop: &ActiveEventLoop,
        font_family: String,
        font_size: f32,
    ) -> Self {
        let physical = window.inner_size();
        let scale = window.scale_factor() as f32;

        let instance = Instance::new(InstanceDescriptor::new_with_display_handle(Box::new(
            event_loop.owned_display_handle(),
        )));
        let adapter = instance
            .request_adapter(&RequestAdapterOptions::default())
            .await
            .expect("no suitable GPU adapter");
        let (device, queue) = adapter
            .request_device(&DeviceDescriptor::default())
            .await
            .expect("failed to create GPU device");

        let surface = instance
            .create_surface(window.clone())
            .expect("failed to create surface");
        let surface_config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format: SURFACE_FORMAT,
            width: physical.width.max(1),
            height: physical.height.max(1),
            present_mode: PresentMode::Fifo,
            alpha_mode: CompositeAlphaMode::Opaque,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &surface_config);

        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, SURFACE_FORMAT);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);
        let modal_text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);

        let metrics = measure_cell(&mut font_system, &font_family, font_size * scale);
        let grid_buf = new_grid_buffer(&mut font_system, metrics);
        let quads = QuadPipeline::new(&device, SURFACE_FORMAT);

        Self {
            instance,
            device,
            queue,
            surface,
            surface_config,
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            modal_text_renderer,
            grid_buf,
            chrome_bufs: Vec::new(),
            labels: Vec::new(),
            modal_bufs: Vec::new(),
            modal_labels: Vec::new(),
            link: None,
            quads,
            under: Vec::new(),
            over: Vec::new(),
            modal_under: Vec::new(),
            modal_over: Vec::new(),
            text: String::new(),
            spans: Vec::new(),
            font_family,
            base_font_size: font_size,
            sidebar_logical: ui::SIDEBAR_DEFAULT,
            metrics,
            theme: Theme::new(
                themes::resolve(themes::DEFAULT_DARK).unwrap_or_default(),
                MIN_CONTRAST,
            ),
            layout: Layout::new(scale, ui::SIDEBAR_DEFAULT),
            scale,
            window,
        }
    }

    pub fn window(&self) -> &Arc<Window> {
        &self.window
    }

    pub fn padding(&self) -> f32 {
        PADDING * self.scale
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.surface_config.width = width.max(1);
        self.surface_config.height = height.max(1);
        self.surface.configure(&self.device, &self.surface_config);
    }

    pub fn set_scale(&mut self, scale: f32) {
        if (scale - self.scale).abs() < f32::EPSILON {
            return;
        }
        self.scale = scale;
        self.layout = Layout::new(scale, self.sidebar_logical);
        self.metrics = measure_cell(
            &mut self.font_system,
            &self.font_family,
            self.base_font_size * scale,
        );
        self.grid_buf = new_grid_buffer(&mut self.font_system, self.metrics);
        // Chrome buffers are rebuilt with the new metrics on the next frame.
        self.chrome_bufs.clear();
        self.modal_bufs.clear();
    }

    /// Maps a physical cursor position to a grid point plus which half of the
    /// cell it fell in, which is what selection needs to decide whether a
    /// boundary cell is included.
    pub fn grid_point(
        &self,
        x: f32,
        y: f32,
        grid: TermSize,
        display_offset: usize,
    ) -> (Point, Side) {
        let (origin_x, origin_y) = self.grid_origin(grid);
        let col_f = ((x - origin_x) / self.metrics.width).max(0.0);
        let line_f = ((y - origin_y) / self.metrics.height).max(0.0);

        let column = (col_f.floor() as usize).min(grid.columns.saturating_sub(1));
        let line = (line_f.floor() as usize).min(grid.screen_lines.saturating_sub(1));
        let side = if col_f.fract() >= 0.5 {
            Side::Right
        } else {
            Side::Left
        };

        let point = viewport_to_point(display_offset, Point::new(line, Column(column)));
        (point, side)
    }

    /// Sets the link to underline. Returns whether it changed.
    pub fn set_link(&mut self, link: Option<(Line, std::ops::Range<usize>)>) -> bool {
        if link == self.link {
            return false;
        }
        self.link = link;
        true
    }

    /// Changes the terminal font size, remeasuring the cell grid.
    pub fn set_font_size(&mut self, logical: f32) {
        let clamped = logical.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if (clamped - self.base_font_size).abs() < 0.01 {
            return;
        }
        self.base_font_size = clamped;
        self.metrics = measure_cell(
            &mut self.font_system,
            &self.font_family,
            clamped * self.scale,
        );
        self.grid_buf = new_grid_buffer(&mut self.font_system, self.metrics);
    }

    pub fn font_size(&self) -> f32 {
        self.base_font_size
    }

    /// Sets the sidebar width from a logical-pixel value, clamped by
    /// `Layout`. Returns whether the effective width actually changed, so the
    /// caller can skip re-laying out the ptys when dragging past a limit.
    pub fn set_sidebar_width(&mut self, logical: f32) -> bool {
        let clamped = logical.clamp(ui::SIDEBAR_MIN, ui::SIDEBAR_MAX);
        if (clamped - self.sidebar_logical).abs() < 0.5 {
            return false;
        }
        self.sidebar_logical = clamped;
        self.layout = Layout::new(self.scale, clamped);
        true
    }

    /// Applies the light or dark palette to match system appearance.
    pub fn set_appearance(&mut self, dark: bool) {
        let name = if dark {
            themes::DEFAULT_DARK
        } else {
            themes::DEFAULT_LIGHT
        };
        if let Some(palette) = themes::resolve(name) {
            self.theme.set_palette(palette);
        }
    }

    /// Grid dimensions that fit the area left of the sidebar, in cells.
    pub fn grid_size(&self) -> TermSize {
        let pad = self.padding();
        let usable_w =
            (self.surface_config.width as f32 - self.layout.sidebar_width - pad * 2.0).max(0.0);
        let usable_h = (self.surface_config.height as f32 - pad * 2.0).max(0.0);
        TermSize {
            columns: ((usable_w / self.metrics.width).floor() as usize)
                .max(alacritty_terminal::term::MIN_COLUMNS),
            screen_lines: ((usable_h / self.metrics.height).floor() as usize)
                .max(alacritty_terminal::term::MIN_SCREEN_LINES),
        }
    }

    /// Origin of the grid, with leftover pixels split evenly instead of
    /// dumped at the right and bottom edges. Cheap, and most of what makes
    /// the window read as composed rather than as a raw buffer.
    fn grid_origin(&self, grid: TermSize) -> (f32, f32) {
        let used_w = grid.columns as f32 * self.metrics.width;
        let used_h = grid.screen_lines as f32 * self.metrics.height;
        let avail_w = self.surface_config.width as f32 - self.layout.sidebar_width;
        let avail_h = self.surface_config.height as f32;
        (
            self.layout.sidebar_width + ((avail_w - used_w) / 2.0).max(self.padding()),
            ((avail_h - used_h) / 2.0).max(self.padding()),
        )
    }

    /// `breathe` is a 0..1 triangle-free sine ramp driving the attention
    /// pulse; callers pass a constant when nothing is waiting.
    pub fn render(
        &mut self,
        sessions: &[Session],
        active: usize,
        breathe: f32,
        find: Option<&FindState>,
        palette: Option<&PaletteState>,
    ) {
        let Some(session) = sessions.get(active) else {
            return;
        };

        self.modal_under.clear();
        self.modal_over.clear();
        self.modal_labels.clear();

        let grid = self.grid_size();
        let (grid_x, grid_y) = self.grid_origin(grid);
        let pad = self.padding();
        let (screen_w, screen_h) = (
            self.surface_config.width as f32,
            self.surface_config.height as f32,
        );

        self.build_grid(session, grid_x, grid_y, screen_w, screen_h);
        self.build_chrome(sessions, active, screen_h, breathe);

        // Find-in-buffer: highlight matches and render the search bar.
        if let Some(find) = find {
            let display_offset = session.term.lock().grid().display_offset();
            self.build_find_highlights(find, grid_x, grid_y, display_offset, grid);
            self.build_find_bar(find, grid_x, grid_y, screen_w);
        }

        // Agent & Command Palette / Visual History overlay
        if let Some(palette) = palette {
            self.build_palette(palette, screen_w, screen_h);
        }

        self.sync_chrome_buffers();
        self.sync_modal_buffers();

        let Self {
            device,
            queue,
            viewport,
            atlas,
            text_renderer,
            modal_text_renderer,
            grid_buf,
            chrome_bufs,
            labels,
            modal_bufs,
            modal_labels,
            font_system,
            swash_cache,
            theme,
            surface_config,
            ..
        } = self;

        viewport.update(
            queue,
            Resolution {
                width: surface_config.width,
                height: surface_config.height,
            },
        );

        let fg = theme.fg();
        let default_color = TextColor::rgb(fg[0], fg[1], fg[2]);
        let mut areas: Vec<TextArea> = Vec::with_capacity(labels.len() + 1);
        areas.push(TextArea {
            buffer: grid_buf,
            left: grid_x,
            top: grid_y,
            scale: 1.0,
            bounds: TextBounds {
                left: grid_x as i32,
                top: grid_y as i32,
                right: (screen_w - pad) as i32,
                bottom: (screen_h - pad) as i32,
            },
            default_color,
            custom_glyphs: &[],
        });
        for (label, buffer) in labels.iter().zip(chrome_bufs.iter()) {
            areas.push(TextArea {
                buffer,
                left: label.x,
                top: label.y,
                scale: 1.0,
                bounds: TextBounds {
                    left: label.x as i32,
                    top: label.y as i32,
                    right: (label.x + label.max_width) as i32,
                    bottom: (label.y + label.line_height * 1.5 * self.scale) as i32,
                },
                default_color: TextColor::rgb(label.color[0], label.color[1], label.color[2]),
                custom_glyphs: &[],
            });
        }

        if let Err(err) = text_renderer.prepare(
            device, queue, font_system, atlas, viewport, areas, swash_cache,
        ) {
            log::warn!("text prepare failed: {err:?}");
            return;
        }

        let mut modal_areas: Vec<TextArea> = Vec::with_capacity(modal_labels.len());
        for (label, buffer) in modal_labels.iter().zip(modal_bufs.iter()) {
            modal_areas.push(TextArea {
                buffer,
                left: label.x,
                top: label.y,
                scale: 1.0,
                bounds: TextBounds {
                    left: label.x as i32,
                    top: label.y as i32,
                    right: (label.x + label.max_width) as i32,
                    bottom: (label.y + label.line_height * 1.5 * self.scale) as i32,
                },
                default_color: TextColor::rgb(label.color[0], label.color[1], label.color[2]),
                custom_glyphs: &[],
            });
        }
        if !modal_areas.is_empty() {
            if let Err(err) = modal_text_renderer.prepare(
                device, queue, font_system, atlas, viewport, modal_areas, swash_cache,
            ) {
                log::warn!("modal text prepare failed: {err:?}");
            }
        }

        self.quads.prepare(
            &self.device,
            &self.queue,
            &self.under,
            &self.over,
            &self.modal_under,
            &self.modal_over,
            [screen_w, screen_h],
        );

        let frame = match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) => frame,
            CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => {
                self.window.request_redraw();
                return;
            },
            CurrentSurfaceTexture::Outdated | CurrentSurfaceTexture::Suboptimal(_) => {
                self.surface.configure(&self.device, &self.surface_config);
                self.window.request_redraw();
                return;
            },
            CurrentSurfaceTexture::Lost => {
                self.surface = self
                    .instance
                    .create_surface(self.window.clone())
                    .expect("failed to recreate surface");
                self.surface.configure(&self.device, &self.surface_config);
                self.window.request_redraw();
                return;
            },
            CurrentSurfaceTexture::Validation => {
                log::error!("surface validation error");
                return;
            },
        };

        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor { label: None });
        {
            let bg = linear_rgba(self.theme.bg(), 1.0);
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("frame"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(wgpu::Color {
                            r: bg[0] as f64,
                            g: bg[1] as f64,
                            b: bg[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            self.quads.render_under(&mut pass);
            if let Err(err) = self
                .text_renderer
                .render(&self.atlas, &self.viewport, &mut pass)
            {
                log::warn!("text render failed: {err:?}");
            }
            self.quads.render_over(&mut pass);

            if !self.modal_under.is_empty() || !self.modal_labels.is_empty() {
                self.quads.render_modal_under(&mut pass);
                if !self.modal_labels.is_empty() {
                    if let Err(err) = self
                        .modal_text_renderer
                        .render(&self.atlas, &self.viewport, &mut pass)
                    {
                        log::warn!("modal text render failed: {err:?}");
                    }
                }
                self.quads.render_modal_over(&mut pass);
            }
        }

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        self.atlas.trim();
    }

    /// Walks the visible grid once, producing background quads, the text to
    /// shape, and the style spans covering it.
    fn build_grid(
        &mut self,
        session: &Session,
        grid_x: f32,
        grid_y: f32,
        screen_w: f32,
        screen_h: f32,
    ) {
        let Self {
            text,
            spans,
            under,
            over,
            grid_buf,
            font_system,
            font_family,
            metrics,
            theme,
            link,
            ..
        } = self;

        text.clear();
        spans.clear();
        under.clear();
        over.clear();
        let theme_fg = theme.fg();

        let term = session.term.lock();
        let content = term.renderable_content();
        let display_offset = content.display_offset;
        let colors = content.colors;
        let selection = content.selection;
        let cursor_point = content.cursor.point;
        let cursor_shape = content.cursor.shape;

        // Merge adjacent cells sharing a background color into one quad.
        let mut bg_run: Option<(usize, usize, usize, Rgb8)> = None; // row, start col, len, color
        let flush_bg = |run: &mut Option<(usize, usize, usize, Rgb8)>, under: &mut Vec<Quad>| {
            if let Some((row, start, len, color)) = run.take() {
                under.push(Quad::new(
                    grid_x + start as f32 * metrics.width,
                    grid_y + row as f32 * metrics.height,
                    len as f32 * metrics.width,
                    metrics.height,
                    linear_rgba(color, 1.0),
                ));
            }
        };

        let mut last_row = 0usize;

        for indexed in content.display_iter {
            let Some(viewport_point) = point_to_viewport(display_offset, indexed.point) else {
                continue;
            };
            let row = viewport_point.line;
            let col = viewport_point.column.0;
            let cell = indexed.cell;

            // Spacer cells belong to the wide char to their left.
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }

            if row != last_row {
                flush_bg(&mut bg_run, under);
                while last_row < row {
                    push_char(text, spans, '\n', theme_fg, Flags::empty());
                    last_row += 1;
                }
            }

            let mut fg = theme.resolve(cell.fg, colors);
            let mut bg = theme.resolve(cell.bg, colors);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if selection
                .is_some_and(|range| range.contains_cell(&indexed, cursor_point, cursor_shape))
            {
                bg = theme.palette.selection_bg;
            }

            if cell.flags.contains(Flags::HIDDEN) {
                fg = bg;
            } else {
                // Guarantee the text clears the readability floor against
                // whatever background it actually landed on.
                fg = theme.enforce_contrast(fg, bg);
            }

            // Background quads are only worth emitting where they differ from
            // the cleared surface.
            if bg != theme.bg() {
                match &mut bg_run {
                    Some((r, start, len, color))
                        if *r == row && *color == bg && *start + *len == col =>
                    {
                        *len += 1;
                    },
                    _ => {
                        flush_bg(&mut bg_run, under);
                        bg_run = Some((row, col, 1, bg));
                    },
                }
            } else {
                flush_bg(&mut bg_run, under);
            }

            let c = if cell.c == '\0' { ' ' } else { cell.c };
            push_char(text, spans, c, fg, cell.flags);
        }
        flush_bg(&mut bg_run, under);

        // Cursor, drawn over the text so the glyph underneath stays readable.
        if content.cursor.shape != CursorShape::Hidden {
            if let Some(vp) = point_to_viewport(display_offset, content.cursor.point) {
                let x = grid_x + vp.column.0 as f32 * metrics.width;
                let y = grid_y + vp.line as f32 * metrics.height;
                let (w, h) = (metrics.width, metrics.height);
                let thickness = (2.0 * (h / 20.0).max(1.0)).round();
                let color = theme.palette.cursor;
                match content.cursor.shape {
                    CursorShape::Block => {
                        over.push(Quad::new(x, y, w, h, linear_rgba(color, 0.55)));
                    },
                    CursorShape::Beam => {
                        over.push(Quad::new(x, y, thickness, h, linear_rgba(color, 1.0)));
                    },
                    CursorShape::Underline => {
                        over.push(Quad::new(
                            x,
                            y + h - thickness,
                            w,
                            thickness,
                            linear_rgba(color, 1.0),
                        ));
                    },
                    CursorShape::HollowBlock => {
                        let c = linear_rgba(color, 1.0);
                        over.push(Quad::new(x, y, w, thickness, c));
                        over.push(Quad::new(x, y + h - thickness, w, thickness, c));
                        over.push(Quad::new(x, y, thickness, h, c));
                        over.push(Quad::new(x + w - thickness, y, thickness, h, c));
                    },
                    CursorShape::Hidden => {},
                }
            }
        }

        // Underline the hovered link, in the text color so it reads as part
        // of the text rather than as chrome.
        if let Some((line, range)) = link.clone() {
            if let Some(vp) = point_to_viewport(display_offset, Point::new(line, Column(range.start)))
            {
                let thickness = (metrics.height / 16.0).max(1.0).round();
                over.push(Quad::new(
                    grid_x + vp.column.0 as f32 * metrics.width,
                    grid_y + vp.line as f32 * metrics.height + metrics.height - thickness,
                    range.len() as f32 * metrics.width,
                    thickness,
                    linear_rgba(theme_fg, 1.0),
                ));
            }
        }

        drop(term);

        // Shape the grid. Spans tile `text` exactly, so concatenating them
        // reproduces it.
        let default_attrs = Attrs::new().family(Family::Name(font_family.as_str()));
        let styled: Vec<(&str, Attrs)> = spans
            .iter()
            .map(|span| {
                let mut attrs = Attrs::new()
                    .family(Family::Name(font_family.as_str()))
                    .color(TextColor::rgb(span.color[0], span.color[1], span.color[2]));
                if span.flags.contains(Flags::BOLD) {
                    attrs = attrs.weight(Weight::BOLD);
                }
                if span.flags.contains(Flags::ITALIC) {
                    attrs = attrs.style(Style::Italic);
                }
                (&text[span.start..span.end], attrs)
            })
            .collect();

        grid_buf.set_size(
            Some((screen_w - grid_x).max(1.0)),
            Some((screen_h - grid_y).max(1.0)),
        );
        grid_buf.set_rich_text(styled, &default_attrs, Shaping::Advanced, None);
        grid_buf.shape_until_scroll(font_system, false);
    }

    /// Highlights find-in-buffer matches on the visible grid.
    ///
    /// All matches get a subdued background; the current match gets a brighter
    /// one so the user can see where they are in the result list.
    fn build_find_highlights(
        &mut self,
        find: &FindState,
        grid_x: f32,
        grid_y: f32,
        display_offset: usize,
        grid: TermSize,
    ) {
        if find.query.is_empty() || find.matches.is_empty() {
            return;
        }

        let current_idx = find.current;
        let highlight_all = mix(self.theme.palette.ansi[3], self.theme.bg(), 0.70);
        let highlight_current = self.theme.palette.ansi[3]; // yellow

        for (i, m) in find.matches.iter().enumerate() {
            let Some(vp) = point_to_viewport(
                display_offset,
                Point::new(m.line, Column(m.start_col)),
            ) else {
                continue;
            };
            // Skip matches outside the visible viewport.
            if vp.line >= grid.screen_lines {
                continue;
            }
            let x = grid_x + vp.column.0 as f32 * self.metrics.width;
            let y = grid_y + vp.line as f32 * self.metrics.height;
            let w = (m.end_col - m.start_col) as f32 * self.metrics.width;
            let color = if i == current_idx {
                highlight_current
            } else {
                highlight_all
            };
            let alpha = if i == current_idx { 0.45 } else { 0.22 };
            self.over.push(Quad::new(x, y, w, self.metrics.height, linear_rgba(color, alpha)));
        }
    }

    /// Renders the find bar: a floating input area at the top-right of the
    /// terminal grid, showing the search query and match status.
    fn build_find_bar(
        &mut self,
        find: &FindState,
        _grid_x: f32,
        grid_y: f32,
        screen_w: f32,
    ) {
        let scale = self.scale;
        let chrome = self.theme.chrome;

        // Dimensions in logical pixels.
        let bar_h = 28.0 * scale;
        let bar_w = 260.0 * scale;
        let margin = 8.0 * scale;
        let bar_x = screen_w - bar_w - margin;
        let bar_y = grid_y;
        let inner_pad = 8.0 * scale;

        // Background and border rendered in the modal overlay layer.
        self.modal_under.push(Quad::rounded(
            bar_x,
            bar_y,
            bar_w,
            bar_h,
            4.0 * scale,
            linear_rgba(chrome.surface, 0.96),
        ));
        self.modal_under.push(Quad::rounded(
            bar_x,
            bar_y,
            bar_w,
            bar_h,
            4.0 * scale,
            linear_rgba(chrome.border, 0.40),
        ));

        // Cursor indicator: a thin beam in the query text.
        let char_w = CHROME_FONT_SIZE * scale * 0.6;
        let chars_before = find.query[..find.cursor_pos].chars().count() as f32;
        let cursor_x = bar_x + inner_pad + chars_before * char_w;
        let cursor_y = bar_y + 5.0 * scale;
        let cursor_h = bar_h - 10.0 * scale;
        self.modal_over.push(Quad::new(
            cursor_x,
            cursor_y,
            (1.5 * scale).max(1.0),
            cursor_h,
            linear_rgba(chrome.text_primary, 0.8),
        ));

        // Query label.
        let status = find.status();
        let display = if find.query.is_empty() {
            "Find…".to_string()
        } else {
            find.query.clone()
        };
        let query_color = if find.query.is_empty() {
            chrome.text_tertiary
        } else {
            chrome.text_primary
        };

        self.modal_labels.push(Label {
            text: display,
            x: bar_x + inner_pad,
            y: bar_y + (bar_h - CHROME_LINE_HEIGHT * scale) / 2.0,
            max_width: bar_w - inner_pad * 2.0 - 60.0 * scale,
            color: query_color,
            font_size: CHROME_FONT_SIZE,
            line_height: CHROME_LINE_HEIGHT,
        });

        // Status label ("3 of 42" / "No matches").
        if !status.is_empty() {
            self.modal_labels.push(Label {
                text: status,
                x: bar_x + bar_w - inner_pad - 55.0 * scale,
                y: bar_y + (bar_h - CHROME_LINE_HEIGHT * scale) / 2.0,
                max_width: 55.0 * scale,
                color: chrome.text_tertiary,
                font_size: ui::SUMMARY_SIZE,
                line_height: CHROME_LINE_HEIGHT,
            });
        }
    }

    /// Renders the Arc/Dia-style visual fuzzy command and history palette overlay.
    fn build_palette(&mut self, palette: &PaletteState, screen_w: f32, screen_h: f32) {
        let scale = self.scale;
        let chrome = self.theme.chrome;

        // 1. Semi-transparent dark backdrop dims the terminal behind the modal
        self.modal_under.push(Quad::new(
            0.0,
            0.0,
            screen_w,
            screen_h,
            [0.0, 0.0, 0.0, 0.50],
        ));

        // 2. Centered Floating Card
        let card_w = (560.0 * scale).min(screen_w - 32.0 * scale);
        let card_h = (380.0 * scale).min(screen_h - 48.0 * scale);
        let card_x = (screen_w - card_w) / 2.0;
        let card_y = ((screen_h - card_h) / 2.0 - 24.0 * scale).max(16.0 * scale);
        let corner_radius = 10.0 * scale;

        // Card surface and crisp antialiased border
        self.modal_under.push(Quad::rounded(
            card_x,
            card_y,
            card_w,
            card_h,
            corner_radius,
            linear_rgba(chrome.surface, 0.98),
        ));
        self.modal_under.push(Quad::rounded(
            card_x,
            card_y,
            card_w,
            card_h,
            corner_radius,
            linear_rgba(chrome.border, 0.60),
        ));

        // 3. Search Input Header
        let input_h = 44.0 * scale;
        let input_pad_x = 16.0 * scale;

        // Mode badge indicator
        let mode_label = match palette.mode {
            PaletteMode::Commands => "AGENT & COMMAND PALETTE",
            PaletteMode::History => "VISUAL HISTORY SEARCH",
        };
        self.modal_labels.push(Label {
            text: mode_label.to_string(),
            x: card_x + input_pad_x,
            y: card_y + 7.0 * scale,
            max_width: card_w - input_pad_x * 2.0,
            color: chrome.text_tertiary,
            font_size: 9.0,
            line_height: 11.0,
        });

        // Prompt symbol
        self.modal_labels.push(Label {
            text: ">".to_string(),
            x: card_x + input_pad_x,
            y: card_y + 20.0 * scale,
            max_width: 14.0 * scale,
            color: chrome.text_primary,
            font_size: 14.0,
            line_height: 18.0,
        });

        // Search text / placeholder
        let placeholder = match palette.mode {
            PaletteMode::Commands => "Search agents, actions, prompts, or history…",
            PaletteMode::History => "Search recent shell commands…",
        };
        let (display_text, text_color) = if palette.query.is_empty() {
            (placeholder.to_string(), chrome.text_tertiary)
        } else {
            (palette.query.clone(), chrome.text_primary)
        };

        let text_x = card_x + input_pad_x + 16.0 * scale;
        self.modal_labels.push(Label {
            text: display_text,
            x: text_x,
            y: card_y + 20.0 * scale,
            max_width: card_w - input_pad_x * 2.0 - 20.0 * scale,
            color: text_color,
            font_size: 13.0,
            line_height: 18.0,
        });

        // Cursor beam
        if !palette.query.is_empty() {
            let char_w = 13.0 * scale * 0.58;
            let chars_before = palette.query[..palette.cursor_pos].chars().count() as f32;
            let cur_x = text_x + chars_before * char_w;
            self.modal_over.push(Quad::new(
                cur_x,
                card_y + 22.0 * scale,
                (1.5 * scale).max(1.0),
                16.0 * scale,
                linear_rgba(chrome.text_primary, 0.9),
            ));
        }

        // Header bottom divider
        self.modal_under.push(Quad::new(
            card_x,
            card_y + input_h,
            card_w,
            scale,
            linear_rgba(chrome.border, 0.35),
        ));

        // 4. Results List
        let list_top = card_y + input_h + 6.0 * scale;
        let row_h = 38.0 * scale;
        let row_margin = 8.0 * scale;
        let row_w = card_w - row_margin * 2.0;
        let footer_h = 28.0 * scale;
        let available_list_h = card_h - input_h - footer_h - 12.0 * scale;
        let max_visible = (available_list_h / row_h).floor() as usize;

        let total = palette.filtered_items.len();
        let start_idx = if palette.selected >= max_visible {
            palette.selected + 1 - max_visible
        } else {
            0
        };
        let end_idx = (start_idx + max_visible).min(total);

        if total == 0 {
            self.modal_labels.push(Label {
                text: "No matching commands or agents".to_string(),
                x: card_x + 24.0 * scale,
                y: list_top + 16.0 * scale,
                max_width: card_w - 48.0 * scale,
                color: chrome.text_tertiary,
                font_size: 12.5,
                line_height: 16.0,
            });
        }

        for (slot, idx) in (start_idx..end_idx).enumerate() {
            let item = &palette.filtered_items[idx];
            let row_x = card_x + row_margin;
            let row_y = list_top + slot as f32 * row_h;
            let is_selected = idx == palette.selected;

            // Selection rounded pill background
            if is_selected {
                self.modal_under.push(Quad::rounded(
                    row_x,
                    row_y,
                    row_w,
                    row_h,
                    6.0 * scale,
                    linear_rgba(chrome.surface_raised, 1.0),
                ));
            }

            // Dot indicator: use item.icon_color or category accent
            let dot_x = row_x + 10.0 * scale;
            let dot_y = row_y + (row_h - 7.0 * scale) / 2.0;
            if let Some(color) = item.icon_color {
                self.modal_under.push(Quad::circle(
                    dot_x,
                    dot_y,
                    7.0 * scale,
                    linear_rgba(color, 1.0),
                ));
            } else {
                let default_dot = match item.category {
                    PaletteCategory::History => [0x6b, 0x6b, 0x6b],
                    PaletteCategory::Action => [0x6c, 0x9e, 0xd9],
                    PaletteCategory::Prompt => [0xe0, 0xb5, 0x5f],
                    _ => chrome.text_tertiary,
                };
                self.modal_under.push(Quad::circle(
                    dot_x,
                    dot_y,
                    5.0 * scale,
                    linear_rgba(default_dot, 0.70),
                ));
            }

            let title_x = row_x + 24.0 * scale;
            let title_y = row_y + 4.0 * scale;
            let title_color = if is_selected {
                chrome.text_primary
            } else {
                chrome.text_secondary
            };

            // Badge width calculation
            let badge_w = if item.badge.is_some() { 76.0 * scale } else { 0.0 };
            let max_text_w = row_w - 32.0 * scale - badge_w;

            self.modal_labels.push(Label {
                text: item.title.clone(),
                x: title_x,
                y: title_y,
                max_width: max_text_w,
                color: title_color,
                font_size: 12.0,
                line_height: 15.0,
            });

            if let Some(ref sub) = item.subtitle {
                self.modal_labels.push(Label {
                    text: sub.clone(),
                    x: title_x,
                    y: title_y + 15.0 * scale,
                    max_width: max_text_w,
                    color: chrome.text_tertiary,
                    font_size: 10.0,
                    line_height: 13.0,
                });
            }

            if let Some(ref badge) = item.badge {
                self.modal_labels.push(Label {
                    text: badge.clone(),
                    x: row_x + row_w - badge_w,
                    y: row_y + (row_h - 14.0 * scale) / 2.0,
                    max_width: badge_w,
                    color: if is_selected { chrome.text_secondary } else { chrome.text_tertiary },
                    font_size: 9.5,
                    line_height: 14.0,
                });
            }
        }

        // 5. Footer Hint Bar
        let footer_y = card_y + card_h - footer_h;
        self.modal_under.push(Quad::new(
            card_x,
            footer_y,
            card_w,
            scale,
            linear_rgba(chrome.border, 0.30),
        ));

        let hint = "↵ run / select  •  Tab insert at prompt  •  ↑↓ navigate  •  esc dismiss";
        self.modal_labels.push(Label {
            text: hint.to_string(),
            x: card_x + 16.0 * scale,
            y: footer_y + (footer_h - 14.0 * scale) / 2.0,
            max_width: card_w - 32.0 * scale,
            color: chrome.text_tertiary,
            font_size: 10.0,
            line_height: 14.0,
        });
    }

    /// Emits the sidebar.
    ///
    /// Each state dimension gets its own visual channel so they compose
    /// without competing: hue carries agent identity, dot *form* carries
    /// activity, the row background carries focus, and motion plus an accent
    /// bar carry attention. Nothing else in the sidebar is saturated, which is
    /// what keeps an agent's dot readable at a glance with six tabs open.
    fn build_chrome(&mut self, sessions: &[Session], active: usize, screen_h: f32, breathe: f32) {
        let layout = self.layout;
        let scale = self.scale;
        let chrome = self.theme.chrome;
        let labels = &mut self.labels;
        let under = &mut self.under;

        labels.clear();

        under.push(Quad::new(
            0.0,
            0.0,
            layout.sidebar_width,
            screen_h,
            linear_rgba(chrome.surface, 1.0),
        ));
        under.push(Quad::new(
            layout.sidebar_width - scale,
            0.0,
            scale,
            screen_h,
            linear_rgba(chrome.border, 1.0),
        ));

        let label_x = layout.side_margin + layout.dot_inset + layout.dot_active + 8.0 * scale;

        labels.push(Label {
            text: "+  New Tab".to_string(),
            x: label_x,
            y: layout.new_tab_y() + (layout.new_tab_height - ui::LABEL_LINE * scale) / 2.0,
            max_width: layout.row_width(),
            color: chrome.text_tertiary,
            font_size: ui::LABEL_SIZE,
            line_height: ui::LABEL_LINE,
        });

        for (index, session) in sessions.iter().enumerate() {
            let row_x = layout.side_margin;
            let row_y = layout.row_y(index);
            let row_w = layout.row_width();
            let is_active = index == active;
            let hue = session.agent().dot_color();

            // Focus.
            if is_active {
                under.push(Quad::rounded(
                    row_x,
                    row_y,
                    row_w,
                    layout.row_height,
                    layout.row_radius,
                    linear_rgba(chrome.surface_raised, 1.0),
                ));
            }

            // Attention. The single animation in the app: an agent that has
            // gone quiet in a tab you aren't looking at.
            let pulse = if session.needs_attention {
                0.55 + 0.45 * breathe
            } else {
                1.0
            };
            if session.needs_attention {
                let bar_h = layout.row_height * 0.62;
                under.push(Quad::rounded(
                    row_x,
                    row_y + (layout.row_height - bar_h) / 2.0,
                    layout.accent_width,
                    bar_h,
                    layout.accent_width * 0.5,
                    linear_rgba(hue, pulse),
                ));
            }

            // Activity, by dot form.
            let center_y = row_y + layout.row_text_top + layout.label_line / 2.0;
            let dot_x = row_x + layout.dot_inset;
            match session.activity {
                Activity::Idle => {
                    let d = layout.dot_idle;
                    under.push(Quad::circle(
                        dot_x + (layout.dot_active - d) / 2.0,
                        center_y - d / 2.0,
                        d,
                        linear_rgba(hue, 0.40),
                    ));
                },
                Activity::Working => {
                    let d = layout.dot_active;
                    under.push(Quad::circle(
                        dot_x,
                        center_y - d / 2.0,
                        d,
                        linear_rgba(hue, 1.0),
                    ));
                },
                Activity::Waiting => {
                    // A ring, drawn as a filled disc with the row's own
                    // background punched back out of the middle.
                    let d = layout.dot_active;
                    let inner = (d - layout.ring_stroke * 2.0).max(1.0);
                    let behind = if is_active {
                        chrome.surface_raised
                    } else {
                        chrome.surface
                    };
                    under.push(Quad::circle(dot_x, center_y - d / 2.0, d, linear_rgba(hue, pulse)));
                    under.push(Quad::circle(
                        dot_x + layout.ring_stroke,
                        center_y - inner / 2.0,
                        inner,
                        linear_rgba(behind, 1.0),
                    ));
                },
            }

            // A count badge, when the tool reports sub-agents of its own,
            // sits between the label and the close button. Neutral, because
            // hue belongs to identity.
            let badge = session.agent_count().map(|n| n.to_string());
            let badge_width = if badge.is_some() {
                ui::BADGE_WIDTH * scale
            } else {
                0.0
            };
            let text_width =
                (row_w - layout.close_width - badge_width - label_x + row_x).max(1.0);

            if let Some(badge) = badge {
                labels.push(Label {
                    text: badge,
                    x: row_x + row_w - layout.close_width - badge_width,
                    y: row_y + layout.row_text_top,
                    max_width: badge_width,
                    color: chrome.text_secondary,
                    font_size: ui::SUMMARY_SIZE,
                    line_height: ui::LABEL_LINE,
                });
            }

            labels.push(Label {
                text: session.label().to_string(),
                x: label_x,
                y: row_y + layout.row_text_top,
                max_width: text_width,
                color: if is_active || session.needs_attention {
                    chrome.text_primary
                } else {
                    chrome.text_secondary
                },
                font_size: ui::LABEL_SIZE,
                line_height: ui::LABEL_LINE,
            });

            // Second line: what this tab is actually doing. Kept on the
            // dimmest token so the sidebar still reads calm and the attention
            // pulse remains the only loud thing in it.
            let summary = session.summary();
            if !summary.is_empty() {
                labels.push(Label {
                    text: summary.to_string(),
                    x: label_x,
                    y: row_y + layout.row_text_top + layout.label_line,
                    max_width: text_width,
                    color: chrome.text_tertiary,
                    font_size: ui::SUMMARY_SIZE,
                    line_height: ui::SUMMARY_LINE,
                });
            }

            labels.push(Label {
                text: "\u{00d7}".to_string(),
                x: row_x + row_w - layout.close_width + 6.0 * scale,
                y: row_y + layout.row_text_top,
                max_width: layout.close_width,
                color: chrome.text_tertiary,
                font_size: ui::LABEL_SIZE,
                line_height: ui::LABEL_LINE,
            });
        }
    }

    /// Shapes chrome labels, growing the buffer pool as tabs are added.
    fn sync_chrome_buffers(&mut self) {
        let fallback = Metrics::new(
            CHROME_FONT_SIZE * self.scale,
            CHROME_LINE_HEIGHT * self.scale,
        );

        while self.chrome_bufs.len() < self.labels.len() {
            let mut buffer = TextBuffer::new(&mut self.font_system, fallback);
            buffer.set_wrap(Wrap::None);
            // Trailing ellipsis at the real pixel width, rather than clipping
            // a glyph in half at the row edge.
            buffer.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
            self.chrome_bufs.push(buffer);
        }
        self.chrome_bufs.truncate(self.labels.len());

        let attrs = Attrs::new().family(Family::SansSerif);
        for (label, buffer) in self.labels.iter().zip(self.chrome_bufs.iter_mut()) {
            let metrics = Metrics::new(
                label.font_size * self.scale,
                label.line_height * self.scale,
            );
            buffer.set_metrics(metrics);
            buffer.set_size(Some(label.max_width), Some(metrics.line_height * 1.5));
            buffer.set_text(&label.text, &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(&mut self.font_system, false);
        }
    }

    /// Shapes modal overlay labels (command palette, find bar).
    fn sync_modal_buffers(&mut self) {
        let fallback = Metrics::new(
            CHROME_FONT_SIZE * self.scale,
            CHROME_LINE_HEIGHT * self.scale,
        );

        while self.modal_bufs.len() < self.modal_labels.len() {
            let mut buffer = TextBuffer::new(&mut self.font_system, fallback);
            buffer.set_wrap(Wrap::None);
            buffer.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
            self.modal_bufs.push(buffer);
        }
        self.modal_bufs.truncate(self.modal_labels.len());

        let attrs = Attrs::new().family(Family::SansSerif);
        for (label, buffer) in self.modal_labels.iter().zip(self.modal_bufs.iter_mut()) {
            let metrics = Metrics::new(
                label.font_size * self.scale,
                label.line_height * self.scale,
            );
            buffer.set_metrics(metrics);
            buffer.set_size(Some(label.max_width), Some(metrics.line_height * 1.5));
            buffer.set_text(&label.text, &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(&mut self.font_system, false);
        }
    }
}

fn new_grid_buffer(font_system: &mut FontSystem, metrics: CellMetrics) -> TextBuffer {
    let mut buffer = TextBuffer::new(font_system, Metrics::new(metrics.font_size, metrics.height));
    buffer.set_wrap(Wrap::None);
    // Pin every glyph to the cell advance so the text grid lines up with the
    // background quads regardless of the font's natural metrics.
    buffer.set_monospace_width(Some(metrics.width));
    buffer
}

/// Appends a char, extending the current span when the style matches so the
/// span list stays as short as possible.
fn push_char(text: &mut String, spans: &mut Vec<Span>, c: char, color: Rgb8, flags: Flags) {
    let start = text.len();
    text.push(c);
    let end = text.len();

    // Newlines carry no style; folding them into the previous span keeps the
    // span list contiguous without splitting runs unnecessarily.
    let style_matches = |span: &Span| c == '\n' || (span.color == color && span.flags == flags);

    match spans.last_mut() {
        Some(span) if span.end == start && style_matches(span) => span.end = end,
        _ => spans.push(Span {
            start,
            end,
            color,
            flags,
        }),
    }
}

/// Derives cell metrics from the font's own advance width so the grid matches
/// whatever monospace face is configured.
fn measure_cell(font_system: &mut FontSystem, font_family: &str, font_size: f32) -> CellMetrics {
    let line_height = (font_size * 1.32).round().max(1.0);
    let mut probe = TextBuffer::new(font_system, Metrics::new(font_size, line_height));
    probe.set_wrap(Wrap::None);

    const SAMPLE: usize = 16;
    let sample = "M".repeat(SAMPLE);
    probe.set_text(
        &sample,
        &Attrs::new().family(Family::Name(font_family)),
        Shaping::Advanced,
        None,
    );
    probe.shape_until_scroll(font_system, false);

    let width = probe
        .layout_runs()
        .next()
        .map(|run| run.line_w / SAMPLE as f32)
        .filter(|w| *w > 0.0)
        .unwrap_or(font_size * 0.6);

    CellMetrics {
        width,
        height: line_height,
        font_size,
    }
}
