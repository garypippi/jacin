//! Mirror of the Neovim buffer lines (from `nvim_buf_attach` line events)
//!
//! Semantic copy of the buffer, used to commit on IME off without an RPC
//! (Neovim may be blocked in getchar then).

/// Buffer lines, kept in sync by `nvim_buf_lines_event`
#[derive(Debug, Clone, PartialEq)]
pub struct BufferMirror {
    lines: Vec<String>,
}

impl Default for BufferMirror {
    fn default() -> Self {
        // A Neovim buffer always has at least one line
        Self {
            lines: vec![String::new()],
        }
    }
}

impl BufferMirror {
    /// Replace lines [first, last) with `lines`; `last = None` means to the
    /// end of the buffer (initial content sent on attach).
    pub fn apply(&mut self, first: usize, last: Option<usize>, lines: Vec<String>) {
        let len = self.lines.len();
        let first = first.min(len);
        let last = last.map_or(len, |l| l.clamp(first, len));
        self.lines.splice(first..last, lines);
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
    }

    /// Whole buffer is a single empty line
    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    /// All lines joined with "\n" (the text to commit)
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn starts_empty() {
        let buf = BufferMirror::default();
        assert!(buf.is_empty());
        assert_eq!(buf.text(), "");
    }

    #[test]
    fn initial_content_replaces_all() {
        let mut buf = BufferMirror::default();
        buf.apply(0, None, lines(&["a", "b"]));
        assert_eq!(buf.text(), "a\nb");
        assert!(!buf.is_empty());
    }

    #[test]
    fn change_insert_and_delete_lines() {
        let mut buf = BufferMirror::default();
        buf.apply(0, None, lines(&["a"]));
        // Edit line 0
        buf.apply(0, Some(1), lines(&["ab"]));
        // <CR> at end: line 0 replaced by "ab" and "" inserted
        buf.apply(0, Some(1), lines(&["ab", ""]));
        assert_eq!(buf.text(), "ab\n");
        // Insert a line before line 1
        buf.apply(1, Some(1), lines(&["x"]));
        assert_eq!(buf.text(), "ab\nx\n");
        // ggdG: delete everything
        buf.apply(0, Some(3), vec![]);
        assert!(buf.is_empty());
    }

    #[test]
    fn out_of_range_is_clamped() {
        let mut buf = BufferMirror::default();
        buf.apply(5, Some(9), lines(&["z"]));
        assert_eq!(buf.text(), "\nz");
    }
}
