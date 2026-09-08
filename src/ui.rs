//! Sidebar layout and hit testing.
//!
//! All constants are in logical pixels; `Layout` scales them once per frame so
//! rendering and mouse handling can't disagree about where anything is.

pub const SIDEBAR_WIDTH: f32 = 200.0;
pub const ROW_HEIGHT: f32 = 32.0;
pub const ROW_GAP: f32 = 2.0;
pub const SIDE_MARGIN: f32 = 6.0;
pub const NEW_TAB_HEIGHT: f32 = 26.0;
pub const NEW_TAB_GAP: f32 = 8.0;
/// Width of the clickable close-button area at the right edge of a row.
pub const CLOSE_WIDTH: f32 = 22.0;
pub const DOT_INSET: f32 = 11.0;
/// Accent bar shown on rows whose agent is waiting on you.
pub const ACCENT_WIDTH: f32 = 2.0;
/// Row corner radius. Borrowed from macOS source-list geometry.
pub const ROW_RADIUS: f32 = 6.0;

/// Dot sizes per activity state. Form, not just opacity, distinguishes them,
/// so activity stays legible independently of the agent's hue.
pub const DOT_IDLE: f32 = 4.0;
pub const DOT_ACTIVE: f32 = 6.0;
/// Stroke width of the hollow ring that marks "waiting on you".
pub const RING_STROKE: f32 = 2.0;

/// Distance from the top of the sidebar to its first control.
///
/// With a full-size content view on macOS the sidebar runs up behind the
/// traffic lights, so the first row has to clear them. Other platforms keep
/// their own decorations and need only breathing room.
pub const fn top_inset() -> f32 {
    if cfg!(target_os = "macos") {
        52.0
    } else {
        12.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    NewTab,
    Tab(usize),
    Close(usize),
    Grid,
}

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub sidebar_width: f32,
    pub row_height: f32,
    pub row_gap: f32,
    pub side_margin: f32,
    pub top_inset: f32,
    pub new_tab_height: f32,
    pub new_tab_gap: f32,
    pub close_width: f32,
    pub dot_inset: f32,
    pub accent_width: f32,
    pub row_radius: f32,
    pub dot_idle: f32,
    pub dot_active: f32,
    pub ring_stroke: f32,
}

impl Layout {
    pub fn new(scale: f32) -> Self {
        Self {
            sidebar_width: SIDEBAR_WIDTH * scale,
            row_height: ROW_HEIGHT * scale,
            row_gap: ROW_GAP * scale,
            side_margin: SIDE_MARGIN * scale,
            top_inset: top_inset() * scale,
            new_tab_height: NEW_TAB_HEIGHT * scale,
            new_tab_gap: NEW_TAB_GAP * scale,
            close_width: CLOSE_WIDTH * scale,
            dot_inset: DOT_INSET * scale,
            accent_width: ACCENT_WIDTH * scale,
            row_radius: ROW_RADIUS * scale,
            dot_idle: DOT_IDLE * scale,
            dot_active: DOT_ACTIVE * scale,
            ring_stroke: RING_STROKE * scale,
        }
    }

    pub fn new_tab_y(&self) -> f32 {
        self.top_inset
    }

    pub fn first_row_y(&self) -> f32 {
        self.top_inset + self.new_tab_height + self.new_tab_gap
    }

    pub fn row_y(&self, index: usize) -> f32 {
        self.first_row_y() + index as f32 * (self.row_height + self.row_gap)
    }

    pub fn row_width(&self) -> f32 {
        self.sidebar_width - self.side_margin * 2.0
    }

    /// Maps a physical-pixel cursor position to whatever it's over.
    pub fn hit_test(&self, x: f32, y: f32, tab_count: usize) -> Hit {
        if x >= self.sidebar_width {
            return Hit::Grid;
        }

        let new_tab_y = self.new_tab_y();
        if y >= new_tab_y && y < new_tab_y + self.new_tab_height {
            return Hit::NewTab;
        }

        let first = self.first_row_y();
        if y < first {
            return Hit::Grid;
        }

        let stride = self.row_height + self.row_gap;
        let index = ((y - first) / stride).floor() as usize;
        if index >= tab_count {
            return Hit::Grid;
        }
        // Reject the gap between rows.
        if y - first - index as f32 * stride > self.row_height {
            return Hit::Grid;
        }

        let row_right = self.side_margin + self.row_width();
        if x >= row_right - self.close_width && x < row_right {
            Hit::Close(index)
        } else {
            Hit::Tab(index)
        }
    }
}
