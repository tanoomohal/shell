//! Sidebar layout and hit testing.
//!
//! All constants are in logical pixels; `Layout` scales them once per frame so
//! rendering and mouse handling can't disagree about where anything is.

pub const SIDEBAR_DEFAULT: f32 = 220.0;
pub const SIDEBAR_MIN: f32 = 168.0;
pub const SIDEBAR_MAX: f32 = 420.0;
/// Half-width of the grab zone straddling the divider.
pub const DIVIDER_GRAB: f32 = 4.0;

/// Rows carry two lines: the process label and a summary of the tab's last
/// output.
pub const ROW_HEIGHT: f32 = 44.0;
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

/// Inset from the top of a row to its first text line.
pub const ROW_TEXT_TOP: f32 = 5.0;
pub const LABEL_SIZE: f32 = 12.5;
pub const LABEL_LINE: f32 = 16.0;
pub const SUMMARY_SIZE: f32 = 10.5;
pub const SUMMARY_LINE: f32 = 14.0;

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
    /// The draggable boundary between sidebar and terminal.
    Divider,
    Grid,
}

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub sidebar_width: f32,
    pub divider_grab: f32,
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
    pub row_text_top: f32,
    pub label_line: f32,
    pub dot_idle: f32,
    pub dot_active: f32,
    pub ring_stroke: f32,
}

impl Layout {
    /// `sidebar_logical` is the user-resizable sidebar width, in logical
    /// pixels, before clamping.
    pub fn new(scale: f32, sidebar_logical: f32) -> Self {
        let sidebar = sidebar_logical.clamp(SIDEBAR_MIN, SIDEBAR_MAX);
        Self {
            sidebar_width: sidebar * scale,
            divider_grab: DIVIDER_GRAB * scale,
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
            row_text_top: ROW_TEXT_TOP * scale,
            label_line: LABEL_LINE * scale,
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
        // The divider straddles the boundary, so it is tested before the
        // sidebar/grid split rather than inside either side.
        if (x - self.sidebar_width).abs() <= self.divider_grab {
            return Hit::Divider;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout::new(1.0, SIDEBAR_DEFAULT)
    }

    #[test]
    fn divider_is_grabbable_from_both_sides() {
        let l = layout();
        let w = l.sidebar_width;
        assert_eq!(l.hit_test(w, 300.0, 2), Hit::Divider);
        assert_eq!(l.hit_test(w - DIVIDER_GRAB + 0.5, 300.0, 2), Hit::Divider);
        assert_eq!(l.hit_test(w + DIVIDER_GRAB - 0.5, 300.0, 2), Hit::Divider);
        // Just outside the grab zone is ordinary terminal area.
        assert_eq!(l.hit_test(w + DIVIDER_GRAB + 2.0, 300.0, 2), Hit::Grid);
    }

    #[test]
    fn rows_and_close_buttons_hit_test() {
        let l = layout();
        let y = l.row_y(1) + l.row_height / 2.0;
        assert_eq!(l.hit_test(l.side_margin + 40.0, y, 3), Hit::Tab(1));
        let close_x = l.side_margin + l.row_width() - CLOSE_WIDTH / 2.0;
        assert_eq!(l.hit_test(close_x, y, 3), Hit::Close(1));
    }

    #[test]
    fn clicks_past_the_last_tab_fall_through() {
        let l = layout();
        let y = l.row_y(5) + l.row_height / 2.0;
        assert_eq!(l.hit_test(40.0, y, 2), Hit::Grid);
    }

    #[test]
    fn sidebar_width_is_clamped() {
        assert_eq!(Layout::new(1.0, 20.0).sidebar_width, SIDEBAR_MIN);
        assert_eq!(Layout::new(1.0, 9000.0).sidebar_width, SIDEBAR_MAX);
        // Scale multiplies after clamping.
        assert_eq!(Layout::new(2.0, SIDEBAR_MIN).sidebar_width, SIDEBAR_MIN * 2.0);
    }
}
