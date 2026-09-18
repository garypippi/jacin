//! Mirror of Neovim's global grid (ext_linegrid)
//!
//! Display-only state: committed text is always read from the Neovim
//! buffer, never derived from the grid.

use std::collections::HashMap;

use crate::neovim::{GridEvent, HlAttr};

/// A single screen cell
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    /// Cell text ("" for the right half of a double-width char)
    pub text: String,
    /// Highlight id
    pub hl: u64,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            text: " ".to_string(),
            hl: 0,
        }
    }
}

/// Default colors from `default_colors_set` (0xRRGGBB)
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DefaultColors {
    pub fg: u32,
    pub bg: u32,
    pub sp: u32,
}

#[derive(Debug, Default)]
pub struct Grid {
    width: usize,
    height: usize,
    /// Row-major cells (width * height)
    cells: Vec<Cell>,
    /// Cursor position (row, col)
    pub cursor: (usize, usize),
    hl_attrs: HashMap<u64, HlAttr>,
    pub default_colors: DefaultColors,
}

impl Grid {
    /// Whether no grid_resize has been received yet
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn apply(&mut self, event: GridEvent) {
        match event {
            GridEvent::Resize { width, height } => self.resize(width, height),
            GridEvent::Clear => self.cells.fill(Cell::default()),
            GridEvent::CursorGoto { row, col } => self.cursor = (row, col),
            GridEvent::Line {
                row,
                col_start,
                cells,
            } => {
                if row >= self.height {
                    return;
                }
                let mut col = col_start;
                for cell in cells {
                    for _ in 0..cell.repeat {
                        if col >= self.width {
                            break;
                        }
                        self.cells[row * self.width + col] = Cell {
                            text: cell.text.clone(),
                            hl: cell.hl,
                        };
                        col += 1;
                    }
                }
            }
            GridEvent::Scroll {
                top,
                bot,
                left,
                right,
                rows,
            } => self.scroll(top, bot, left, right, rows),
            GridEvent::HlAttrDefine { id, attr } => {
                self.hl_attrs.insert(id, attr);
            }
            GridEvent::DefaultColors { fg, bg, sp } => {
                self.default_colors = DefaultColors { fg, bg, sp };
            }
        }
    }

    /// Resize keeping the overlapping region
    fn resize(&mut self, width: usize, height: usize) {
        let mut cells = vec![Cell::default(); width * height];
        for row in 0..height.min(self.height) {
            for col in 0..width.min(self.width) {
                cells[row * width + col] = self.cells[row * self.width + col].clone();
            }
        }
        self.cells = cells;
        self.width = width;
        self.height = height;
    }

    /// Copy rows within the scroll region; the scrolled-in area is
    /// redrawn by following grid_line events.
    fn scroll(&mut self, top: usize, bot: usize, left: usize, right: usize, rows: i64) {
        let bot = bot.min(self.height);
        let right = right.min(self.width);
        let shift = rows.unsigned_abs() as usize;
        if shift == 0 || top + shift >= bot {
            return;
        }
        let copy_row = |cells: &mut Vec<Cell>, dst: usize, src: usize| {
            for col in left..right {
                cells[dst * self.width + col] = cells[src * self.width + col].clone();
            }
        };
        if rows > 0 {
            // Move up: dst = src - shift
            for dst in top..bot - shift {
                copy_row(&mut self.cells, dst, dst + shift);
            }
        } else {
            // Move down: dst = src + shift
            for dst in (top + shift..bot).rev() {
                copy_row(&mut self.cells, dst, dst - shift);
            }
        }
    }

    /// Text of a row (double-width chars appear once)
    pub fn row_text(&self, row: usize) -> String {
        if row >= self.height {
            return String::new();
        }
        let start = row * self.width;
        self.cells[start..start + self.width]
            .iter()
            .map(|c| c.text.as_str())
            .collect()
    }

    /// Byte offset of screen column `col` within `row_text(row)`
    pub fn byte_offset(&self, row: usize, col: usize) -> usize {
        if row >= self.height {
            return 0;
        }
        let start = row * self.width;
        self.cells[start..start + col.min(self.width)]
            .iter()
            .map(|c| c.text.len())
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neovim::protocol::GridCell;

    fn line(row: usize, col_start: usize, cells: &[(&str, u64, usize)]) -> GridEvent {
        GridEvent::Line {
            row,
            col_start,
            cells: cells
                .iter()
                .map(|&(text, hl, repeat)| GridCell {
                    text: text.into(),
                    hl,
                    repeat,
                })
                .collect(),
        }
    }

    fn grid(width: usize, height: usize) -> Grid {
        let mut g = Grid::default();
        g.apply(GridEvent::Resize { width, height });
        g
    }

    #[test]
    fn line_writes_cells_with_repeat() {
        let mut g = grid(6, 2);
        g.apply(line(1, 1, &[("a", 1, 1), ("-", 2, 3)]));
        assert_eq!(g.row_text(1), " a--- ");
        assert_eq!(g.row_text(0), "      ");
    }

    #[test]
    fn line_is_clipped_to_width() {
        let mut g = grid(3, 1);
        g.apply(line(0, 1, &[("x", 0, 10)]));
        assert_eq!(g.row_text(0), " xx");
    }

    #[test]
    fn double_width_chars_and_byte_offset() {
        let mut g = grid(6, 1);
        g.apply(line(0, 0, &[("あ", 0, 1), ("", 0, 1), ("b", 0, 1)]));
        assert_eq!(g.row_text(0), "あb   ");
        assert_eq!(g.byte_offset(0, 2), 3); // after "あ"
        assert_eq!(g.byte_offset(0, 3), 4); // after "あb"
    }

    #[test]
    fn scroll_up_and_down() {
        let mut g = grid(1, 4);
        for (row, t) in ["a", "b", "c", "d"].iter().enumerate() {
            g.apply(line(row, 0, &[(t, 0, 1)]));
        }
        g.apply(GridEvent::Scroll {
            top: 0,
            bot: 4,
            left: 0,
            right: 1,
            rows: 1,
        });
        let rows: Vec<_> = (0..4).map(|r| g.row_text(r)).collect();
        assert_eq!(rows, ["b", "c", "d", "d"]);

        g.apply(GridEvent::Scroll {
            top: 0,
            bot: 4,
            left: 0,
            right: 1,
            rows: -2,
        });
        let rows: Vec<_> = (0..4).map(|r| g.row_text(r)).collect();
        assert_eq!(rows, ["b", "c", "b", "c"]);
    }

    #[test]
    fn resize_keeps_overlap_and_clear_blanks() {
        let mut g = grid(3, 1);
        g.apply(line(0, 0, &[("abc", 0, 1)]));
        g.apply(GridEvent::Resize {
            width: 2,
            height: 2,
        });
        assert_eq!(g.row_text(0), "abc "); // first cell kept, second blank
        g.apply(GridEvent::Clear);
        assert_eq!(g.row_text(0), "  ");
    }

    #[test]
    fn out_of_range_events_are_ignored() {
        let mut g = grid(2, 1);
        g.apply(line(5, 0, &[("x", 0, 1)]));
        g.apply(GridEvent::Scroll {
            top: 0,
            bot: 9,
            left: 0,
            right: 9,
            rows: 3,
        });
        assert_eq!(g.row_text(0), "  ");
        assert_eq!(g.row_text(5), "");
    }
}
