//! Keysym-to-Vim notation conversion
//!
//! Pure functions for converting XKB keysyms and modifier state into Vim key notation.
//! No I/O or mutable state — all functions are side-effect free.

use xkbcommon::xkb;

/// Map keysym to its Vim special-key name (e.g. `Return` → `"CR"`).
///
/// Only returns names for keys that need `<>` wrapping in Vim notation.
/// Returns `None` for letters, digits, and other printable characters.
fn special_key_name(keysym: xkb::Keysym) -> Option<&'static str> {
    use xkbcommon::xkb::Keysym;

    match keysym {
        Keysym::Return | Keysym::KP_Enter => Some("CR"),
        Keysym::BackSpace => Some("BS"),
        Keysym::Tab => Some("Tab"),
        Keysym::Escape => Some("Esc"),
        Keysym::space => Some("Space"),
        Keysym::Left => Some("Left"),
        Keysym::Right => Some("Right"),
        Keysym::Up => Some("Up"),
        Keysym::Down => Some("Down"),
        _ => None,
    }
}

/// Map keysym to a lowercase letter (a-z), if applicable.
fn keysym_to_letter(keysym: xkb::Keysym) -> Option<char> {
    use xkbcommon::xkb::Keysym;

    if keysym.raw() >= Keysym::a.raw() && keysym.raw() <= Keysym::z.raw() {
        Some((keysym.raw() - Keysym::a.raw() + b'a' as u32) as u8 as char)
    } else {
        None
    }
}

/// Returns `true` if `utf8` contains at least one printable (non-control) character.
pub(crate) fn is_printable(utf8: &str) -> bool {
    !utf8.is_empty() && !utf8.chars().all(char::is_control)
}

/// Convert an XKB keysym + modifiers to Vim notation.
///
/// Returns `None` if the key has no Vim representation (e.g. bare modifier keys).
pub(crate) fn keysym_to_vim(
    ctrl: bool,
    alt: bool,
    keysym: xkb::Keysym,
    utf8: &str,
) -> Option<String> {
    // Handle Alt combinations: <A-key>
    if alt {
        if let Some(name) = special_key_name(keysym) {
            return Some(format!("<A-{name}>"));
        }
        if let Some(c) = keysym_to_letter(keysym) {
            return Some(format!("<A-{c}>"));
        }
        if is_printable(utf8) {
            let escaped = utf8.replace('<', "lt");
            return Some(format!("<A-{escaped}>"));
        }
        return None;
    }

    // Handle Ctrl combinations: <C-key>
    if ctrl {
        if let Some(name) = special_key_name(keysym) {
            return Some(format!("<C-{name}>"));
        }
        if let Some(c) = keysym_to_letter(keysym) {
            return Some(format!("<C-{c}>"));
        }
        return None;
    }

    // No modifier: special keys get <> wrapper, printable chars returned as-is
    if let Some(name) = special_key_name(keysym) {
        return Some(format!("<{name}>"));
    }
    if is_printable(utf8) {
        // Escape '<' as '<lt>' for nvim_input (bare '<' starts a key sequence)
        Some(utf8.replace('<', "<lt>"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{is_printable, keysym_to_letter, keysym_to_vim, special_key_name};
    use xkbcommon::xkb::Keysym;

    // ── special_key_name ──

    #[test]
    fn special_key_name_return_variants() {
        assert_eq!(special_key_name(Keysym::Return), Some("CR"));
        assert_eq!(special_key_name(Keysym::KP_Enter), Some("CR"));
    }

    #[test]
    fn special_key_name_all_mapped() {
        assert_eq!(special_key_name(Keysym::BackSpace), Some("BS"));
        assert_eq!(special_key_name(Keysym::Tab), Some("Tab"));
        assert_eq!(special_key_name(Keysym::Escape), Some("Esc"));
        assert_eq!(special_key_name(Keysym::space), Some("Space"));
        assert_eq!(special_key_name(Keysym::Left), Some("Left"));
        assert_eq!(special_key_name(Keysym::Right), Some("Right"));
        assert_eq!(special_key_name(Keysym::Up), Some("Up"));
        assert_eq!(special_key_name(Keysym::Down), Some("Down"));
    }

    #[test]
    fn special_key_name_non_special_returns_none() {
        assert_eq!(special_key_name(Keysym::a), None);
        assert_eq!(special_key_name(Keysym::_1), None);
        assert_eq!(special_key_name(Keysym::Shift_L), None);
    }

    // ── keysym_to_letter ──

    #[test]
    fn keysym_to_letter_lowercase_range() {
        assert_eq!(keysym_to_letter(Keysym::a), Some('a'));
        assert_eq!(keysym_to_letter(Keysym::m), Some('m'));
        assert_eq!(keysym_to_letter(Keysym::z), Some('z'));
    }

    #[test]
    fn keysym_to_letter_non_letter_returns_none() {
        assert_eq!(keysym_to_letter(Keysym::A), None);
        assert_eq!(keysym_to_letter(Keysym::_1), None);
        assert_eq!(keysym_to_letter(Keysym::Return), None);
        assert_eq!(keysym_to_letter(Keysym::space), None);
    }

    // ── is_printable ──

    #[test]
    fn is_printable_ascii() {
        assert!(is_printable("a"));
        assert!(is_printable("Z"));
        assert!(is_printable("1"));
        assert!(is_printable("<"));
    }

    #[test]
    fn is_printable_multibyte() {
        assert!(is_printable("あ"));
        assert!(is_printable("漢"));
    }

    #[test]
    fn is_printable_empty_and_control() {
        assert!(!is_printable(""));
        assert!(!is_printable("\x00"));
        assert!(!is_printable("\x1b"));
    }

    // ── keysym_to_vim: no modifiers ──

    #[test]
    fn printable_ascii() {
        assert_eq!(
            keysym_to_vim(false, false, Keysym::a, "a"),
            Some("a".into())
        );
        assert_eq!(
            keysym_to_vim(false, false, Keysym::z, "z"),
            Some("z".into())
        );
    }

    #[test]
    fn uppercase_via_utf8() {
        assert_eq!(
            keysym_to_vim(false, false, Keysym::A, "A"),
            Some("A".into())
        );
    }

    #[test]
    fn digit_keys() {
        assert_eq!(
            keysym_to_vim(false, false, Keysym::_0, "0"),
            Some("0".into())
        );
        assert_eq!(
            keysym_to_vim(false, false, Keysym::_9, "9"),
            Some("9".into())
        );
    }

    #[test]
    fn special_keys() {
        assert_eq!(
            keysym_to_vim(false, false, Keysym::Return, ""),
            Some("<CR>".into())
        );
        assert_eq!(
            keysym_to_vim(false, false, Keysym::BackSpace, ""),
            Some("<BS>".into())
        );
        assert_eq!(
            keysym_to_vim(false, false, Keysym::Escape, ""),
            Some("<Esc>".into())
        );
        assert_eq!(
            keysym_to_vim(false, false, Keysym::Tab, ""),
            Some("<Tab>".into())
        );
        assert_eq!(
            keysym_to_vim(false, false, Keysym::space, ""),
            Some("<Space>".into())
        );
    }

    #[test]
    fn kp_enter_maps_to_cr() {
        assert_eq!(
            keysym_to_vim(false, false, Keysym::KP_Enter, ""),
            Some("<CR>".into())
        );
    }

    #[test]
    fn arrow_keys() {
        assert_eq!(
            keysym_to_vim(false, false, Keysym::Left, ""),
            Some("<Left>".into())
        );
        assert_eq!(
            keysym_to_vim(false, false, Keysym::Right, ""),
            Some("<Right>".into())
        );
        assert_eq!(
            keysym_to_vim(false, false, Keysym::Up, ""),
            Some("<Up>".into())
        );
        assert_eq!(
            keysym_to_vim(false, false, Keysym::Down, ""),
            Some("<Down>".into())
        );
    }

    #[test]
    fn less_than_escaped() {
        assert_eq!(
            keysym_to_vim(false, false, Keysym::less, "<"),
            Some("<lt>".into())
        );
    }

    #[test]
    fn bare_modifier_returns_none() {
        assert_eq!(keysym_to_vim(false, false, Keysym::Shift_L, ""), None);
        assert_eq!(keysym_to_vim(false, false, Keysym::Shift_R, ""), None);
        assert_eq!(keysym_to_vim(false, false, Keysym::Control_L, ""), None);
        assert_eq!(keysym_to_vim(false, false, Keysym::Control_R, ""), None);
        assert_eq!(keysym_to_vim(false, false, Keysym::Alt_L, ""), None);
        assert_eq!(keysym_to_vim(false, false, Keysym::Super_L, ""), None);
    }

    #[test]
    fn japanese_utf8() {
        assert_eq!(
            keysym_to_vim(false, false, Keysym::NoSymbol, "あ"),
            Some("あ".into())
        );
    }

    #[test]
    fn kanji_utf8() {
        assert_eq!(
            keysym_to_vim(false, false, Keysym::NoSymbol, "漢"),
            Some("漢".into())
        );
    }

    // ── keysym_to_vim: Ctrl modifier ──

    #[test]
    fn ctrl_letter() {
        assert_eq!(
            keysym_to_vim(true, false, Keysym::a, "a"),
            Some("<C-a>".into())
        );
        assert_eq!(
            keysym_to_vim(true, false, Keysym::z, "z"),
            Some("<C-z>".into())
        );
    }

    #[test]
    fn ctrl_special_keys() {
        assert_eq!(
            keysym_to_vim(true, false, Keysym::Return, ""),
            Some("<C-CR>".into())
        );
        assert_eq!(
            keysym_to_vim(true, false, Keysym::BackSpace, ""),
            Some("<C-BS>".into())
        );
        assert_eq!(
            keysym_to_vim(true, false, Keysym::Tab, ""),
            Some("<C-Tab>".into())
        );
        assert_eq!(
            keysym_to_vim(true, false, Keysym::space, ""),
            Some("<C-Space>".into())
        );
    }

    #[test]
    fn ctrl_non_letter_non_special_returns_none() {
        // Ctrl+digit: keysym_to_letter returns None, not special → None
        assert_eq!(keysym_to_vim(true, false, Keysym::_1, "1"), None);
    }

    // ── keysym_to_vim: Alt modifier ──

    #[test]
    fn alt_letter() {
        assert_eq!(
            keysym_to_vim(false, true, Keysym::a, "a"),
            Some("<A-a>".into())
        );
        assert_eq!(
            keysym_to_vim(false, true, Keysym::z, "z"),
            Some("<A-z>".into())
        );
    }

    #[test]
    fn alt_special_keys() {
        assert_eq!(
            keysym_to_vim(false, true, Keysym::Return, ""),
            Some("<A-CR>".into())
        );
        assert_eq!(
            keysym_to_vim(false, true, Keysym::Escape, ""),
            Some("<A-Esc>".into())
        );
    }

    #[test]
    fn alt_printable_utf8() {
        assert_eq!(
            keysym_to_vim(false, true, Keysym::_1, "1"),
            Some("<A-1>".into())
        );
        assert_eq!(
            keysym_to_vim(false, true, Keysym::NoSymbol, "あ"),
            Some("<A-あ>".into())
        );
    }

    #[test]
    fn alt_less_than_escaped() {
        assert_eq!(
            keysym_to_vim(false, true, Keysym::less, "<"),
            Some("<A-lt>".into())
        );
    }

    #[test]
    fn alt_bare_modifier_returns_none() {
        assert_eq!(keysym_to_vim(false, true, Keysym::Shift_L, ""), None);
    }

    // ── keysym_to_vim: Ctrl+Alt (Alt takes priority) ──

    #[test]
    fn ctrl_alt_letter_alt_wins() {
        // When both ctrl and alt are true, alt branch is entered first
        assert_eq!(
            keysym_to_vim(true, true, Keysym::a, "a"),
            Some("<A-a>".into())
        );
    }

    #[test]
    fn ctrl_alt_special_key_alt_wins() {
        assert_eq!(
            keysym_to_vim(true, true, Keysym::Return, ""),
            Some("<A-CR>".into())
        );
    }
}
