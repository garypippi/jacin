//! Cell storage for one Neovim grid (ext_linegrid)
//!
//! Display-only state: committed text is always read from the Neovim
//! buffer, never derived from the grid. `Screen` owns the grids and routes
//! events to them.

use crate::neovim::GridCell;

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

#[derive(Debug, Default)]
pub struct Grid {
    width: usize,
    height: usize,
    /// Row-major cells (width * height)
    cells: Vec<Cell>,
    /// Per row: the screen line continues on the next row (soft wrap)
    wraps: Vec<bool>,
}

impl Grid {
    #[cfg(test)]
    pub fn new(width: usize, height: usize) -> Self {
        let mut grid = Self::default();
        grid.resize(width, height);
        grid
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// Cell at (row, col); None if out of range
    pub fn cell(&self, row: usize, col: usize) -> Option<&Cell> {
        (row < self.height && col < self.width).then(|| &self.cells[row * self.width + col])
    }

    /// Whether `row` wraps onto the next row
    pub fn wraps(&self, row: usize) -> bool {
        self.wraps.get(row).copied().unwrap_or(false)
    }

    pub fn clear(&mut self) {
        self.cells.fill(Cell::default());
        self.wraps.fill(false);
    }

    /// Write cells starting at (row, col_start), clipped to the grid.
    /// `wrap` is only meaningful when the cells reach the last column.
    pub fn put_line(&mut self, row: usize, col_start: usize, cells: Vec<GridCell>, wrap: bool) {
        if row >= self.height {
            return;
        }
        let mut col = col_start;
        'cells: for cell in cells {
            for _ in 0..cell.repeat {
                if col >= self.width {
                    break 'cells;
                }
                self.cells[row * self.width + col] = Cell {
                    text: cell.text.clone(),
                    hl: cell.hl,
                };
                col += 1;
            }
        }
        if col >= self.width {
            self.wraps[row] = wrap;
        }
    }

    /// Resize keeping the overlapping region
    pub fn resize(&mut self, width: usize, height: usize) {
        let mut cells = vec![Cell::default(); width * height];
        for row in 0..height.min(self.height) {
            for col in 0..width.min(self.width) {
                cells[row * width + col] = self.cells[row * self.width + col].clone();
            }
        }
        self.cells = cells;
        self.wraps.resize(height, false);
        self.width = width;
        self.height = height;
    }

    /// Copy rows within the scroll region; the scrolled-in area is
    /// redrawn by following grid_line events.
    pub fn scroll(&mut self, top: usize, bot: usize, left: usize, right: usize, rows: i64) {
        let bot = bot.min(self.height);
        let right = right.min(self.width);
        let shift = rows.unsigned_abs() as usize;
        if shift == 0 || top + shift >= bot {
            return;
        }
        // Wrap flags describe whole rows; move them only for full-width scrolls
        let full_width = left == 0 && right == self.width;
        let width = self.width;
        let mut copy_row = |dst: usize, src: usize| {
            for col in left..right {
                self.cells[dst * width + col] = self.cells[src * width + col].clone();
            }
            if full_width {
                self.wraps[dst] = self.wraps[src];
            }
        };
        if rows > 0 {
            // Move up: dst = src - shift
            for dst in top..bot - shift {
                copy_row(dst, dst + shift);
            }
        } else {
            // Move down: dst = src + shift
            for dst in (top + shift..bot).rev() {
                copy_row(dst, dst - shift);
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

    fn cells(cells: &[(&str, u64, usize)]) -> Vec<GridCell> {
        cells
            .iter()
            .map(|&(text, hl, repeat)| GridCell {
                text: text.into(),
                hl,
                repeat,
            })
            .collect()
    }

    #[test]
    fn line_writes_cells_with_repeat() {
        let mut g = Grid::new(6, 2);
        g.put_line(1, 1, cells(&[("a", 1, 1), ("-", 2, 3)]), false);
        assert_eq!(g.row_text(1), " a--- ");
        assert_eq!(g.row_text(0), "      ");
    }

    #[test]
    fn line_is_clipped_to_width() {
        let mut g = Grid::new(3, 1);
        g.put_line(0, 1, cells(&[("x", 0, 10)]), false);
        assert_eq!(g.row_text(0), " xx");
    }

    #[test]
    fn double_width_chars_and_byte_offset() {
        let mut g = Grid::new(6, 1);
        g.put_line(0, 0, cells(&[("あ", 0, 1), ("", 0, 1), ("b", 0, 1)]), false);
        assert_eq!(g.row_text(0), "あb   ");
        assert_eq!(g.byte_offset(0, 2), 3); // after "あ"
        assert_eq!(g.byte_offset(0, 3), 4); // after "あb"
    }

    #[test]
    fn scroll_up_and_down() {
        let mut g = Grid::new(1, 4);
        for (row, t) in ["a", "b", "c", "d"].iter().enumerate() {
            g.put_line(row, 0, cells(&[(t, 0, 1)]), false);
        }
        g.scroll(0, 4, 0, 1, 1);
        let rows: Vec<_> = (0..4).map(|r| g.row_text(r)).collect();
        assert_eq!(rows, ["b", "c", "d", "d"]);

        g.scroll(0, 4, 0, 1, -2);
        let rows: Vec<_> = (0..4).map(|r| g.row_text(r)).collect();
        assert_eq!(rows, ["b", "c", "b", "c"]);
    }

    #[test]
    fn resize_keeps_overlap_and_clear_blanks() {
        let mut g = Grid::new(3, 1);
        g.put_line(0, 0, cells(&[("abc", 0, 1)]), false);
        g.resize(2, 2);
        assert_eq!(g.row_text(0), "abc "); // first cell kept, second blank
        g.clear();
        assert_eq!(g.row_text(0), "  ");
    }

    #[test]
    fn wrap_flag_set_only_when_reaching_last_column_and_scrolled() {
        let mut g = Grid::new(3, 3);
        g.put_line(0, 0, cells(&[("a", 0, 1)]), true); // does not reach col 2
        assert!(!g.wraps(0));
        g.put_line(0, 0, cells(&[("a", 0, 3)]), true);
        assert!(g.wraps(0));

        g.scroll(0, 3, 0, 3, -1); // full-width scroll moves the flag down
        assert!(g.wraps(1));
        g.clear();
        assert!(!g.wraps(1));
    }

    #[test]
    fn out_of_range_writes_are_ignored() {
        let mut g = Grid::new(2, 1);
        g.put_line(5, 0, cells(&[("x", 0, 1)]), false);
        g.scroll(0, 9, 0, 9, 3);
        assert_eq!(g.row_text(0), "  ");
        assert_eq!(g.row_text(5), "");
    }
}
