//! Find-in-buffer: search through terminal scrollback.
//!
//! Case-insensitive search across the grid and its scrollback. Matches are
//! stored by grid coordinate so the renderer can highlight them without a
//! second pass over the grid, and the match list is cheap to rebuild on
//! every keystroke.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};

/// A single match in the terminal grid.
#[derive(Clone, Debug)]
pub struct FindMatch {
    pub line: Line,
    pub start_col: usize,
    /// Exclusive end column.
    pub end_col: usize,
}

/// State for the find-in-buffer overlay.
pub struct FindState {
    /// Current search query.
    pub query: String,
    /// Cursor position within the query (byte offset).
    pub cursor_pos: usize,
    /// All matches in the grid, ordered top-to-bottom, left-to-right.
    pub matches: Vec<FindMatch>,
    /// Index of the currently focused match.
    pub current: usize,
}

/// Simple case-folding: take the first character of the lowercase form.
/// Correct for all of Latin/Cyrillic/Greek and good enough for a terminal
/// search where exotic ligature folding is not expected.
fn lower(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

impl FindState {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            cursor_pos: 0,
            matches: Vec::new(),
            current: 0,
        }
    }

    /// Create a new find state pre-populated with an initial query (e.g.
    /// from the current selection).
    pub fn with_query(initial: &str) -> Self {
        // Only take the first line of a multi-line selection.
        let first_line = initial.lines().next().unwrap_or("");
        let cursor_pos = first_line.len();
        Self {
            query: first_line.to_string(),
            cursor_pos,
            matches: Vec::new(),
            current: 0,
        }
    }

    // ── Text editing ─────────────────────────────────────────────────

    /// Insert a character at the cursor position.
    pub fn insert(&mut self, c: char) {
        self.query.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    /// Insert a string at the cursor position.
    pub fn insert_str(&mut self, s: &str) {
        self.query.insert_str(self.cursor_pos, s);
        self.cursor_pos += s.len();
    }

    /// Delete the character before the cursor.
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
    }

    /// Delete the character after the cursor.
    pub fn delete_forward(&mut self) {
        if self.cursor_pos >= self.query.len() {
            return;
        }
        let next = self.query[self.cursor_pos..]
            .char_indices()
            .nth(1)
            .map(|(i, _)| self.cursor_pos + i)
            .unwrap_or(self.query.len());
        self.query.drain(self.cursor_pos..next);
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

    /// Delete from cursor to end of query (Ctrl+K).
    pub fn kill_to_end(&mut self) {
        self.query.truncate(self.cursor_pos);
    }

    /// Delete from cursor to start of query (Ctrl+U).
    pub fn kill_to_start(&mut self) {
        self.query.drain(..self.cursor_pos);
        self.cursor_pos = 0;
    }

    /// Delete the word before the cursor (Ctrl+W / Alt+Backspace).
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
    }

    // ── Match navigation ─────────────────────────────────────────────

    /// Advance to the next match, wrapping around.
    pub fn next_match(&mut self) {
        if !self.matches.is_empty() {
            self.current = (self.current + 1) % self.matches.len();
        }
    }

    /// Go back to the previous match, wrapping around.
    pub fn prev_match(&mut self) {
        if !self.matches.is_empty() {
            self.current = if self.current == 0 {
                self.matches.len() - 1
            } else {
                self.current - 1
            };
        }
    }

    /// Returns the currently focused match, if any.
    pub fn current_match(&self) -> Option<&FindMatch> {
        self.matches.get(self.current)
    }

    // ── Search ────────────────────────────────────────────────────────

    /// Re-scan the terminal grid for matches. Call after the query changes
    /// or the active tab switches.
    pub fn search<L: alacritty_terminal::event::EventListener>(
        &mut self,
        term: &alacritty_terminal::term::Term<L>,
    ) {
        self.matches.clear();

        if self.query.is_empty() {
            self.current = 0;
            return;
        }

        let grid = term.grid();
        let cols = grid.columns();
        let topmost = grid.topmost_line();
        let bottom = Line(grid.screen_lines() as i32 - 1);

        let query_lower: Vec<char> = self.query.chars().map(lower).collect();
        let qlen = query_lower.len();

        let mut line = topmost;
        while line <= bottom {
            let row = &grid[line];
            let row_lower: Vec<char> = (0..cols)
                .map(|c| {
                    let ch = row[Column(c)].c;
                    lower(if ch == '\0' { ' ' } else { ch })
                })
                .collect();

            if row_lower.len() >= qlen {
                for start in 0..=row_lower.len() - qlen {
                    if row_lower[start..start + qlen] == query_lower[..] {
                        self.matches.push(FindMatch {
                            line,
                            start_col: start,
                            end_col: start + qlen,
                        });
                    }
                }
            }

            line = Line(line.0 + 1);
        }

        // Clamp current to valid range.
        if self.matches.is_empty() {
            self.current = 0;
        } else {
            self.current = self.current.min(self.matches.len() - 1);
        }
    }

    /// Status text for the find bar, e.g. "3 of 42".
    pub fn status(&self) -> String {
        if self.query.is_empty() {
            String::new()
        } else if self.matches.is_empty() {
            "No matches".to_string()
        } else {
            format!("{} of {}", self.current + 1, self.matches.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_backspace() {
        let mut f = FindState::new();
        f.insert('h');
        f.insert('i');
        assert_eq!(f.query, "hi");
        assert_eq!(f.cursor_pos, 2);
        f.backspace();
        assert_eq!(f.query, "h");
        assert_eq!(f.cursor_pos, 1);
    }

    #[test]
    fn cursor_movement() {
        let mut f = FindState::with_query("hello");
        assert_eq!(f.cursor_pos, 5);
        f.cursor_left();
        assert_eq!(f.cursor_pos, 4);
        f.cursor_home();
        assert_eq!(f.cursor_pos, 0);
        f.cursor_right();
        assert_eq!(f.cursor_pos, 1);
        f.cursor_end();
        assert_eq!(f.cursor_pos, 5);
    }

    #[test]
    fn delete_word() {
        let mut f = FindState::with_query("hello world");
        f.delete_word();
        assert_eq!(f.query, "hello ");
        f.delete_word();
        assert_eq!(f.query, "");
    }

    #[test]
    fn kill_lines() {
        let mut f = FindState::with_query("abcdef");
        f.cursor_pos = 3;
        f.kill_to_end();
        assert_eq!(f.query, "abc");
        f.kill_to_start();
        assert_eq!(f.query, "");
    }

    #[test]
    fn with_query_takes_first_line() {
        let f = FindState::with_query("line one\nline two");
        assert_eq!(f.query, "line one");
    }

    #[test]
    fn match_navigation_wraps() {
        let mut f = FindState::new();
        f.matches = vec![
            FindMatch { line: Line(0), start_col: 0, end_col: 3 },
            FindMatch { line: Line(1), start_col: 0, end_col: 3 },
            FindMatch { line: Line(2), start_col: 0, end_col: 3 },
        ];
        f.current = 2;
        f.next_match();
        assert_eq!(f.current, 0);
        f.prev_match();
        assert_eq!(f.current, 2);
    }

    #[test]
    fn status_text() {
        let mut f = FindState::new();
        assert_eq!(f.status(), "");
        f.query = "test".to_string();
        assert_eq!(f.status(), "No matches");
        f.matches = vec![
            FindMatch { line: Line(0), start_col: 0, end_col: 4 },
            FindMatch { line: Line(1), start_col: 5, end_col: 9 },
        ];
        f.current = 0;
        assert_eq!(f.status(), "1 of 2");
    }

    #[test]
    fn case_folding() {
        assert_eq!(lower('A'), 'a');
        assert_eq!(lower('z'), 'z');
        assert_eq!(lower('É'), 'é');
    }
}
