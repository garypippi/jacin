//! Mirror of Neovim's UI screen with ext_multigrid
//!
//! Grid 1 is the global grid (statusline etc.); each window has its own
//! grid, and floating windows (e.g. nvim-cmp menus) get separate grids with
//! a Neovim-computed screen position. Display-only state.

use std::collections::HashMap;

use super::Grid;
use super::grid::Cell;
use crate::neovim::{GridEvent, HlAttr};

/// Default colors from `default_colors_set` (0xRRGGBB)
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DefaultColors {
    pub fg: u32,
    pub bg: u32,
    pub sp: u32,
}

/// Visible cursor: the grid it is on ("current grid") and its position
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Cursor {
    pub grid: u64,
    pub row: usize,
    pub col: usize,
}

/// Where a window grid is shown
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Placement {
    /// Normal window at (row, col) on the global grid
    Normal { row: usize, col: usize },
    /// Floating window at a Neovim-computed screen position
    Float {
        anchor_grid: u64,
        row: usize,
        col: usize,
        zindex: u64,
    },
}

/// Buffer range shown in a window (zero-based)
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Viewport {
    pub topline: usize,
    pub botline: usize,
    pub curline: usize,
    pub curcol: usize,
    pub line_count: usize,
}

/// Window state attached to a window grid
#[derive(Debug, Clone, Default)]
pub struct Window {
    pub placement: Option<Placement>,
    pub hidden: bool,
    pub viewport: Option<Viewport>,
}

/// A cell resolved for rendering (None colors = theme default)
#[derive(Debug, Clone, PartialEq)]
pub struct StyledCell {
    /// Cell text ("" for the right half of a double-width char)
    pub text: String,
    pub fg: Option<u32>,
    pub bg: Option<u32>,
    pub reverse: bool,
    pub underline: bool,
}

impl StyledCell {
    /// Blank cell with default colors (trimmed at the end of rows)
    fn is_plain_blank(&self) -> bool {
        self.text == " " && self.bg.is_none() && !self.reverse && !self.underline
    }
}

/// Visible buffer rows of the current window, ready for rendering
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WindowView {
    /// Screen rows (trailing plain blanks trimmed)
    pub rows: Vec<Vec<StyledCell>>,
    /// Cursor (row within `rows`, screen column)
    pub cursor: (usize, usize),
}

#[derive(Debug, Default)]
pub struct Screen {
    grids: HashMap<u64, Grid>,
    /// Keyed by grid id (only grids that are windows)
    windows: HashMap<u64, Window>,
    pub cursor: Cursor,
    hl_attrs: HashMap<u64, HlAttr>,
    pub default_colors: DefaultColors,
}

impl Screen {
    pub fn grid(&self, id: u64) -> Option<&Grid> {
        self.grids.get(&id)
    }

    pub fn window(&self, grid: u64) -> Option<&Window> {
        self.windows.get(&grid)
    }

    /// Grid of the window holding the cursor
    pub fn cursor_grid(&self) -> Option<&Grid> {
        self.grid(self.cursor.grid)
    }

    /// Screen rows of the window holding the cursor that show buffer text
    /// (filler rows past the buffer end excluded), at most `max_rows`,
    /// scrolled so that the cursor row is visible.
    pub fn window_view(&self, max_rows: usize) -> Option<WindowView> {
        let grid = self.cursor_grid()?;
        if self.cursor.grid == 1 || max_rows == 0 {
            return None; // no window grid yet
        }
        let lines = self
            .window(self.cursor.grid)
            .and_then(|w| w.viewport)
            .map_or(1, |v| v.line_count.saturating_sub(v.topline))
            .max(1);

        // Each buffer line spans rows until one that does not wrap
        let mut used_rows = 0;
        let mut consumed = 0;
        while used_rows < grid.height() && consumed < lines {
            if !grid.wraps(used_rows) {
                consumed += 1;
            }
            used_rows += 1;
        }
        let used_rows = used_rows.max(self.cursor.row + 1).min(grid.height());
        let first = (self.cursor.row + 1).saturating_sub(max_rows);
        let last = used_rows.min(first + max_rows);

        let rows = (first..last)
            .map(|row| {
                let mut cells: Vec<StyledCell> = (0..grid.width())
                    .filter_map(|col| grid.cell(row, col))
                    .map(|cell| self.style(cell))
                    .collect();
                while cells.last().is_some_and(StyledCell::is_plain_blank) {
                    cells.pop();
                }
                cells
            })
            .collect();
        Some(WindowView {
            rows,
            cursor: (self.cursor.row - first, self.cursor.col),
        })
    }

    fn style(&self, cell: &Cell) -> StyledCell {
        let attr = self.hl_attrs.get(&cell.hl);
        StyledCell {
            text: cell.text.clone(),
            fg: attr.and_then(|a| a.foreground),
            bg: attr.and_then(|a| a.background),
            reverse: attr.is_some_and(|a| a.reverse),
            underline: attr.is_some_and(|a| a.underline || a.undercurl),
        }
    }

    pub fn apply(&mut self, event: GridEvent) {
        match event {
            GridEvent::Resize {
                grid,
                width,
                height,
            } => self.grids.entry(grid).or_default().resize(width, height),
            GridEvent::Clear { grid } => {
                if let Some(g) = self.grids.get_mut(&grid) {
                    g.clear();
                }
            }
            GridEvent::Destroy { grid } => {
                self.grids.remove(&grid);
                self.windows.remove(&grid);
            }
            GridEvent::CursorGoto { grid, row, col } => {
                self.cursor = Cursor { grid, row, col };
            }
            GridEvent::Line {
                grid,
                row,
                col_start,
                cells,
                wrap,
            } => {
                if let Some(g) = self.grids.get_mut(&grid) {
                    g.put_line(row, col_start, cells, wrap);
                }
            }
            GridEvent::Scroll {
                grid,
                top,
                bot,
                left,
                right,
                rows,
            } => {
                if let Some(g) = self.grids.get_mut(&grid) {
                    g.scroll(top, bot, left, right, rows);
                }
            }
            GridEvent::HlAttrDefine { id, attr } => {
                self.hl_attrs.insert(id, attr);
            }
            GridEvent::DefaultColors { fg, bg, sp } => {
                self.default_colors = DefaultColors { fg, bg, sp };
            }
            GridEvent::WinPos { grid, row, col, .. } => {
                let win = self.windows.entry(grid).or_default();
                win.placement = Some(Placement::Normal { row, col });
                win.hidden = false;
            }
            GridEvent::WinFloatPos {
                grid,
                anchor_grid,
                screen_row,
                screen_col,
                zindex,
            } => {
                let win = self.windows.entry(grid).or_default();
                win.placement = Some(Placement::Float {
                    anchor_grid,
                    row: screen_row,
                    col: screen_col,
                    zindex,
                });
                win.hidden = false;
            }
            GridEvent::WinHide { grid } => {
                if let Some(win) = self.windows.get_mut(&grid) {
                    win.hidden = true;
                }
            }
            GridEvent::WinClose { grid } => {
                self.windows.remove(&grid);
            }
            GridEvent::WinViewport {
                grid,
                topline,
                botline,
                curline,
                curcol,
                line_count,
            } => {
                self.windows.entry(grid).or_default().viewport = Some(Viewport {
                    topline,
                    botline,
                    curline,
                    curcol,
                    line_count,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neovim::GridCell;

    fn line(grid: u64, row: usize, text: &str) -> GridEvent {
        GridEvent::Line {
            grid,
            row,
            col_start: 0,
            cells: vec![GridCell {
                text: text.into(),
                hl: 0,
                repeat: 1,
            }],
            wrap: false,
        }
    }

    fn resize(grid: u64, width: usize, height: usize) -> GridEvent {
        GridEvent::Resize {
            grid,
            width,
            height,
        }
    }

    #[test]
    fn events_are_routed_by_grid_id() {
        let mut screen = Screen::default();
        screen.apply(resize(1, 4, 2));
        screen.apply(resize(2, 4, 1));
        screen.apply(line(1, 1, "S"));
        screen.apply(line(2, 0, "a"));
        screen.apply(line(9, 0, "x")); // unknown grid: ignored
        screen.apply(GridEvent::CursorGoto {
            grid: 2,
            row: 0,
            col: 1,
        });

        assert_eq!(screen.grid(1).unwrap().row_text(1), "S   ");
        assert_eq!(screen.cursor_grid().unwrap().row_text(0), "a   ");
        assert_eq!(screen.cursor.col, 1);
    }

    #[test]
    fn float_window_lifecycle() {
        let mut screen = Screen::default();
        screen.apply(resize(5, 15, 1));
        screen.apply(GridEvent::WinFloatPos {
            grid: 5,
            anchor_grid: 1,
            screen_row: 2,
            screen_col: 0,
            zindex: 1001,
        });
        assert_eq!(
            screen.window(5).unwrap().placement,
            Some(Placement::Float {
                anchor_grid: 1,
                row: 2,
                col: 0,
                zindex: 1001
            })
        );

        screen.apply(GridEvent::WinHide { grid: 5 });
        assert!(screen.window(5).unwrap().hidden);

        screen.apply(GridEvent::WinClose { grid: 5 });
        screen.apply(GridEvent::Destroy { grid: 5 });
        assert!(screen.window(5).is_none());
        assert!(screen.grid(5).is_none());
    }

    fn window(lines: &[(&str, bool)], line_count: usize, cursor: (usize, usize)) -> Screen {
        let mut screen = Screen::default();
        screen.apply(resize(1, 6, 5));
        screen.apply(resize(2, 6, 4));
        for (row, (text, wrap)) in lines.iter().enumerate() {
            screen.apply(GridEvent::Line {
                grid: 2,
                row,
                col_start: 0,
                cells: text
                    .chars()
                    .map(|c| GridCell {
                        text: c.to_string(),
                        hl: 0,
                        repeat: 1,
                    })
                    .collect(),
                wrap: *wrap,
            });
        }
        screen.apply(GridEvent::WinViewport {
            grid: 2,
            topline: 0,
            botline: line_count + 1,
            curline: 0,
            curcol: 0,
            line_count,
        });
        screen.apply(GridEvent::CursorGoto {
            grid: 2,
            row: cursor.0,
            col: cursor.1,
        });
        screen
    }

    fn texts(view: &WindowView) -> Vec<String> {
        view.rows
            .iter()
            .map(|r| r.iter().map(|c| c.text.as_str()).collect())
            .collect()
    }

    #[test]
    fn window_view_excludes_filler_rows() {
        let screen = window(&[("ab", false), ("~", false), ("~", false)], 1, (0, 2));
        let view = screen.window_view(8).unwrap();
        assert_eq!(texts(&view), ["ab"]);
        assert_eq!(view.cursor, (0, 2));
    }

    #[test]
    fn window_view_counts_wrapped_rows() {
        // One buffer line wrapped over two rows (line_count = 1)
        let screen = window(&[("abcdef", true), ("gh", false), ("~", false)], 1, (1, 2));
        let view = screen.window_view(8).unwrap();
        assert_eq!(texts(&view), ["abcdef", "gh"]);
    }

    #[test]
    fn window_view_scrolls_to_cursor_row() {
        let screen = window(&[("a", false), ("b", false), ("c", false)], 3, (2, 0));
        let view = screen.window_view(2).unwrap();
        assert_eq!(texts(&view), ["b", "c"]);
        assert_eq!(view.cursor, (1, 0));
    }

    #[test]
    fn window_view_resolves_highlights() {
        let mut screen = window(&[("a", false)], 1, (0, 0));
        screen.apply(GridEvent::HlAttrDefine {
            id: 7,
            attr: HlAttr {
                background: Some(0x123456),
                reverse: true,
                ..HlAttr::default()
            },
        });
        screen.apply(GridEvent::Line {
            grid: 2,
            row: 0,
            col_start: 1,
            cells: vec![GridCell {
                text: " ".into(),
                hl: 7,
                repeat: 1,
            }],
            wrap: false,
        });
        let view = screen.window_view(8).unwrap();
        // Highlighted blank is kept, plain trailing blanks are trimmed
        assert_eq!(view.rows[0].len(), 2);
        assert_eq!(view.rows[0][1].bg, Some(0x123456));
        assert!(view.rows[0][1].reverse);
    }

    #[test]
    fn window_view_none_before_window_grid() {
        let mut screen = Screen::default();
        screen.apply(resize(1, 4, 2));
        screen.apply(GridEvent::CursorGoto {
            grid: 1,
            row: 0,
            col: 0,
        });
        assert!(screen.window_view(8).is_none());
    }

    #[test]
    fn viewport_is_tracked_per_window() {
        let mut screen = Screen::default();
        screen.apply(GridEvent::WinViewport {
            grid: 2,
            topline: 0,
            botline: 3,
            curline: 1,
            curcol: 0,
            line_count: 2,
        });
        assert_eq!(screen.window(2).unwrap().viewport.unwrap().line_count, 2);
    }
}
