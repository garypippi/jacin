//! Mirror of Neovim's UI screen with ext_multigrid
//!
//! Grid 1 is the global grid (statusline etc.); each window has its own
//! grid, and floating windows (e.g. nvim-cmp menus) get separate grids with
//! a Neovim-computed screen position. Display-only state.

use std::collections::HashMap;

use super::Grid;
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
            } => {
                if let Some(g) = self.grids.get_mut(&grid) {
                    g.put_line(row, col_start, cells);
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
