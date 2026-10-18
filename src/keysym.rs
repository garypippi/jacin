//! Keysym-to-Vim notation conversion
//!
//! Pure functions for converting XKB keysyms and modifier state into Vim key notation.
//! No I/O or mutable state — all functions are side-effect free.

use std::borrow::Cow;

use xkbcommon::xkb;
use xkbcommon::xkb::Keysym;

/// Keysyms with a Vim key name, other than F1-F35.
///
/// Vim names come from `:help key-notation` and Neovim's
/// `src/nvim/keycodes.lua` (what `nvim_input` reads). Keysyms with no Vim
/// name (Menu, media keys, ...) are left out. Keypad keys without NumLock get
/// the `k` names; unmapped, Neovim treats them as the plain keys. Keypad keys
/// with NumLock give digits as text, so they need no entry.
const NAMED_KEYS: &[(Keysym, &str)] = &[
    (Keysym::Return, "CR"),
    // <kEnter> would act the same; <CR> keeps one key for Enter
    (Keysym::KP_Enter, "CR"),
    (Keysym::BackSpace, "BS"),
    (Keysym::Tab, "Tab"),
    // What Shift+Tab gives
    (Keysym::ISO_Left_Tab, "Tab"),
    (Keysym::Escape, "Esc"),
    (Keysym::space, "Space"),
    (Keysym::Delete, "Del"),
    (Keysym::Insert, "Insert"),
    (Keysym::Home, "Home"),
    (Keysym::End, "End"),
    (Keysym::Page_Up, "PageUp"),
    (Keysym::Page_Down, "PageDown"),
    (Keysym::Left, "Left"),
    (Keysym::Right, "Right"),
    (Keysym::Up, "Up"),
    (Keysym::Down, "Down"),
    (Keysym::Help, "Help"),
    (Keysym::Undo, "Undo"),
    (Keysym::Find, "Find"),
    (Keysym::Select, "Select"),
    (Keysym::KP_Home, "kHome"),
    (Keysym::KP_End, "kEnd"),
    (Keysym::KP_Page_Up, "kPageUp"),
    (Keysym::KP_Page_Down, "kPageDown"),
    (Keysym::KP_Insert, "kInsert"),
    (Keysym::KP_Delete, "kDel"),
    (Keysym::KP_Left, "kLeft"),
    (Keysym::KP_Right, "kRight"),
    (Keysym::KP_Up, "kUp"),
    (Keysym::KP_Down, "kDown"),
    (Keysym::KP_Begin, "kOrigin"),
];

/// Map keysym to its Vim special-key name (e.g. `Return` → `"CR"`).
///
/// Only returns names for keys that need `<>` wrapping in Vim notation.
/// Returns `None` for letters, digits, and other printable characters.
fn special_key_name(keysym: Keysym) -> Option<Cow<'static, str>> {
    // F1-F35 are consecutive keysyms
    if (Keysym::F1.raw()..=Keysym::F35.raw()).contains(&keysym.raw()) {
        let n = keysym.raw() - Keysym::F1.raw() + 1;
        return Some(Cow::Owned(format!("F{n}")));
    }
    NAMED_KEYS
        .iter()
        .find(|(sym, _)| *sym == keysym)
        .map(|(_, name)| Cow::Borrowed(*name))
}

/// Every Vim key name `special_key_name` can return
#[cfg(test)]
pub(crate) fn special_key_names() -> Vec<String> {
    let f_keys = (1..=35).map(|n| format!("F{n}"));
    let named = NAMED_KEYS.iter().map(|(_, name)| name.to_string());
    let mut names: Vec<String> = named.chain(f_keys).collect();
    names.sort();
    names.dedup();
    names
}

/// Map keysym to a lowercase letter (a-z), if applicable.
fn keysym_to_letter(keysym: Keysym) -> Option<char> {
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
pub(crate) fn keysym_to_vim(ctrl: bool, alt: bool, keysym: Keysym, utf8: &str) -> Option<String> {
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
        // utf8 is already a control character (Ctrl+\ gives "\x1c"), so the
        // character comes from the keysym. Neovim decides what <C-x> means
        // (<C-\><C-n> leaves terminal mode; unmapped <C-1> inserts "1")
        let c = char::from_u32(xkb::keysym_to_utf32(keysym)).filter(|c| !c.is_control())?;
        return Some(if c == '<' {
            "<C-lt>".into()
        } else {
            format!("<C-{c}>")
        });
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

    fn name(keysym: Keysym) -> Option<String> {
        special_key_name(keysym).map(|n| n.into_owned())
    }

    #[test]
    fn special_key_name_table() {
        for (keysym, expected) in [
            (Keysym::Return, "CR"),
            (Keysym::KP_Enter, "CR"),
            (Keysym::BackSpace, "BS"),
            (Keysym::Tab, "Tab"),
            (Keysym::ISO_Left_Tab, "Tab"),
            (Keysym::Escape, "Esc"),
            (Keysym::space, "Space"),
            (Keysym::Delete, "Del"),
            (Keysym::Insert, "Insert"),
            (Keysym::Home, "Home"),
            (Keysym::End, "End"),
            (Keysym::Page_Up, "PageUp"),
            (Keysym::Page_Down, "PageDown"),
            (Keysym::Left, "Left"),
            (Keysym::Right, "Right"),
            (Keysym::Up, "Up"),
            (Keysym::Down, "Down"),
            (Keysym::KP_Home, "kHome"),
            (Keysym::KP_Delete, "kDel"),
            (Keysym::KP_Begin, "kOrigin"),
        ] {
            assert_eq!(name(keysym).as_deref(), Some(expected), "{keysym:?}");
        }
    }

    #[test]
    fn special_key_name_function_keys() {
        assert_eq!(name(Keysym::F1).as_deref(), Some("F1"));
        assert_eq!(name(Keysym::F12).as_deref(), Some("F12"));
        assert_eq!(name(Keysym::F35).as_deref(), Some("F35"));
    }

    #[test]
    fn special_key_name_non_special_returns_none() {
        assert_eq!(name(Keysym::a), None);
        assert_eq!(name(Keysym::_1), None);
        assert_eq!(name(Keysym::KP_1), None);
        assert_eq!(name(Keysym::Shift_L), None);
        assert_eq!(name(Keysym::Menu), None);
    }

    #[test]
    fn named_keys_in_vim_notation() {
        assert_eq!(
            keysym_to_vim(false, false, Keysym::Delete, ""),
            Some("<Del>".into())
        );
        assert_eq!(
            keysym_to_vim(false, false, Keysym::ISO_Left_Tab, ""),
            Some("<Tab>".into())
        );
        assert_eq!(
            keysym_to_vim(true, false, Keysym::Home, ""),
            Some("<C-Home>".into())
        );
        assert_eq!(
            keysym_to_vim(false, true, Keysym::F5, ""),
            Some("<A-F5>".into())
        );
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
    fn ctrl_symbols_and_digits() {
        // Ctrl turns utf8 into a control character; the keysym gives the key
        assert_eq!(
            keysym_to_vim(true, false, Keysym::backslash, "\x1c"),
            Some("<C-\\>".into())
        );
        assert_eq!(
            keysym_to_vim(true, false, Keysym::bracketleft, "\x1b"),
            Some("<C-[>".into())
        );
        assert_eq!(
            keysym_to_vim(true, false, Keysym::_1, "1"),
            Some("<C-1>".into())
        );
        assert_eq!(
            keysym_to_vim(true, false, Keysym::less, "<"),
            Some("<C-lt>".into())
        );
    }

    #[test]
    fn ctrl_modifier_only_returns_none() {
        assert_eq!(keysym_to_vim(true, false, Keysym::Control_L, ""), None);
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
