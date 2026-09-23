//! Vim mode, classified from both `mode_change` (push) and `nvim_get_mode`
//! (pull)
//!
//! The two sources report different detail (completion shows as "ic" in
//! nvim_get_mode but stays "insert" in mode_change; select mode shows as
//! "s" but "visual"), so modes are grouped at the detail both can tell apart.
//! Unknown values fall into `Other`, which is handled conservatively.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Insert mode, also during completion (ic, ix)
    Insert,
    /// Replace and virtual replace mode
    Replace,
    /// Normal mode, also after <C-o> in insert mode (niI)
    Normal,
    OperatorPending,
    /// Visual (charwise, linewise, blockwise) and select mode
    Visual,
    CmdLine,
    /// Anything else (hit-enter prompt, more prompt, terminal, ...)
    Other,
}

impl Mode {
    /// Mode from a `mode_change` UI event name. None for transient states
    /// that don't change the editing mode (mouse hover, showmatch).
    pub fn from_ui(name: &str) -> Option<Self> {
        Some(match name {
            "insert" => Self::Insert,
            "replace" => Self::Replace,
            "normal" => Self::Normal,
            "operator" => Self::OperatorPending,
            // visual_select is visual mode with 'selection' exclusive
            "visual" | "visual_select" => Self::Visual,
            "cmdline_normal" | "cmdline_insert" | "cmdline_replace" => Self::CmdLine,
            "cmdline_hover" | "statusline_hover" | "statusline_drag" | "vsep_hover"
            | "vsep_drag" | "showmatch" => return None,
            _ => Self::Other,
        })
    }

    /// Mode from the `mode` field of `nvim_get_mode`
    pub fn from_get_mode(mode: &str) -> Self {
        match mode {
            "i" | "ic" | "ix" => Self::Insert,
            "R" | "Rc" | "Rx" | "Rv" | "Rvc" | "Rvx" => Self::Replace,
            "n" | "niI" | "niR" | "niV" | "nt" | "ntT" => Self::Normal,
            "no" | "nov" | "noV" | "no\x16" => Self::OperatorPending,
            "v" | "V" | "\x16" | "vs" | "Vs" | "\x16s" | "s" | "S" | "\x13" => Self::Visual,
            "c" | "cr" | "cv" | "cvr" | "ce" => Self::CmdLine,
            _ => Self::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_pull_agree() {
        // Observed pairs (mode_change name, nvim_get_mode) for the same state
        for (ui, pulled) in [
            ("insert", "i"),
            ("insert", "ic"),
            ("insert", "ix"),
            ("normal", "n"),
            ("normal", "niI"),
            ("visual", "v"),
            ("visual", "V"),
            ("visual", "\x16"),
            ("visual", "s"),
            ("visual", "S"),
            ("visual_select", "v"),
            ("replace", "R"),
            ("replace", "Rv"),
            ("operator", "no"),
            ("cmdline_normal", "c"),
        ] {
            assert_eq!(
                Mode::from_ui(ui),
                Some(Mode::from_get_mode(pulled)),
                "{ui} / {pulled:?}"
            );
        }
    }

    #[test]
    fn transient_ui_states_are_ignored() {
        assert_eq!(Mode::from_ui("showmatch"), None);
        assert_eq!(Mode::from_ui("cmdline_hover"), None);
    }

    #[test]
    fn unknown_modes_are_other() {
        assert_eq!(Mode::from_ui("more"), Some(Mode::Other));
        assert_eq!(Mode::from_ui("something_new"), Some(Mode::Other));
        assert_eq!(Mode::from_get_mode("rm"), Mode::Other);
        assert_eq!(Mode::from_get_mode("r?"), Mode::Other);
        assert_eq!(Mode::from_get_mode("!"), Mode::Other);
        assert_eq!(Mode::from_get_mode("t"), Mode::Other);
        assert_eq!(Mode::from_get_mode("ixyz"), Mode::Other);
    }
}
