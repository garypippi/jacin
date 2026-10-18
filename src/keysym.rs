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

/// Modifiers held with a key
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct KeyMods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Super
    pub logo: bool,
}

impl KeyMods {
    /// Modifier prefix in the order keytrans() writes them (`<M-C-S-x>`),
    /// with `A-` for Alt
    fn prefix(self, with_shift: bool) -> String {
        let mut prefix = String::new();
        if self.alt {
            prefix.push_str("A-");
        }
        if self.ctrl {
            prefix.push_str("C-");
        }
        if with_shift && self.shift {
            prefix.push_str("S-");
        }
        prefix
    }
}

/// Returns `true` if `utf8` contains at least one printable (non-control) character.
fn is_printable(utf8: &str) -> bool {
    !utf8.is_empty() && !utf8.chars().all(char::is_control)
}

/// Convert an XKB keysym + modifiers to Vim notation.
///
/// Only writes the key down; what it means is up to Neovim (unmapped
/// `<S-Tab>` acts as `<Tab>`, `<kHome>` as `<Home>`). Returns `None` if the
/// key has no Vim representation (e.g. bare modifier keys).
pub(crate) fn keysym_to_vim(mods: KeyMods, keysym: Keysym, utf8: &str) -> Option<String> {
    // Super combinations are the compositor's: unmapped, Neovim would type
    // <D-x> into the buffer as text
    if mods.logo {
        return None;
    }
    // Named keys carry every modifier: <S-Tab>, <C-S-Left>
    if let Some(name) = special_key_name(keysym) {
        return Some(format!("<{}{name}>", mods.prefix(true)));
    }
    if mods.ctrl || mods.alt {
        // With Ctrl, utf8 is already a control character (Ctrl+\ gives
        // "\x1c"), so the character comes from the keysym. Neovim decides
        // what <C-x> means (<C-\><C-n> leaves terminal mode; unmapped <C-1>
        // inserts "1")
        let c = char::from_u32(xkb::keysym_to_utf32(keysym)).filter(|c| !c.is_control())?;
        // Shift is already in the character (<A-A>, <C-!>). Ctrl ignores
        // case, as in a terminal: Ctrl+Shift+r is <C-r>
        let c = if mods.ctrl { c.to_ascii_lowercase() } else { c };
        let c = if c == '<' {
            "lt".to_string()
        } else {
            c.to_string()
        };
        return Some(format!("<{}{c}>", mods.prefix(false)));
    }
    // Shift alone is in the text already. Escape '<' as '<lt>' for
    // nvim_input (a bare '<' starts a key)
    is_printable(utf8).then(|| utf8.replace('<', "<lt>"))
}

/// The key of a Vim notation without its modifiers: `"CR"` for `<C-S-CR>`.
/// `None` for a plain character.
pub(crate) fn key_name(notation: &str) -> Option<&str> {
    let mut inner = notation.strip_prefix('<')?.strip_suffix('>')?;
    while let Some(rest) = ["A-", "C-", "S-"]
        .iter()
        .find_map(|m| inner.strip_prefix(m))
        .filter(|rest| !rest.is_empty())
    {
        inner = rest;
    }
    Some(inner)
}

#[cfg(test)]
mod tests {
    use super::{KeyMods, is_printable, key_name, keysym_to_vim, special_key_name};
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

    // ── is_printable ──

    #[test]
    fn is_printable_text_and_control() {
        assert!(is_printable("a"));
        assert!(is_printable("<"));
        assert!(is_printable("あ"));
        assert!(!is_printable(""));
        assert!(!is_printable("\x00"));
        assert!(!is_printable("\x1b"));
    }

    // ── keysym_to_vim ──

    const NONE: KeyMods = KeyMods {
        ctrl: false,
        alt: false,
        shift: false,
        logo: false,
    };
    const CTRL: KeyMods = KeyMods { ctrl: true, ..NONE };
    const ALT: KeyMods = KeyMods { alt: true, ..NONE };
    const SHIFT: KeyMods = KeyMods {
        shift: true,
        ..NONE
    };
    const CTRL_ALT: KeyMods = KeyMods {
        ctrl: true,
        alt: true,
        ..NONE
    };
    const CTRL_SHIFT: KeyMods = KeyMods {
        ctrl: true,
        shift: true,
        ..NONE
    };
    const ALT_SHIFT: KeyMods = KeyMods {
        alt: true,
        shift: true,
        ..NONE
    };
    const ALL: KeyMods = KeyMods {
        ctrl: true,
        alt: true,
        shift: true,
        logo: false,
    };
    const LOGO: KeyMods = KeyMods { logo: true, ..NONE };

    fn check(cases: &[(KeyMods, Keysym, &str, Option<&str>)]) {
        for &(mods, keysym, utf8, expected) in cases {
            assert_eq!(
                keysym_to_vim(mods, keysym, utf8).as_deref(),
                expected,
                "{mods:?} {keysym:?} {utf8:?}"
            );
        }
    }

    #[test]
    fn plain_keys() {
        check(&[
            (NONE, Keysym::a, "a", Some("a")),
            (NONE, Keysym::_9, "9", Some("9")),
            (NONE, Keysym::KP_1, "1", Some("1")),
            (NONE, Keysym::less, "<", Some("<lt>")),
            (NONE, Keysym::kana_A, "ア", Some("ア")),
            (NONE, Keysym::Return, "\r", Some("<CR>")),
            (NONE, Keysym::KP_Enter, "\r", Some("<CR>")),
            (NONE, Keysym::space, " ", Some("<Space>")),
            (NONE, Keysym::Delete, "\x7f", Some("<Del>")),
            (NONE, Keysym::KP_Home, "", Some("<kHome>")),
            (NONE, Keysym::F5, "", Some("<F5>")),
        ]);
    }

    #[test]
    fn shift() {
        check(&[
            // In the text already
            (SHIFT, Keysym::A, "A", Some("A")),
            (SHIFT, Keysym::exclam, "!", Some("!")),
            (SHIFT, Keysym::greater, ">", Some(">")),
            // Written for named keys
            (SHIFT, Keysym::ISO_Left_Tab, "\t", Some("<S-Tab>")),
            (SHIFT, Keysym::Return, "\r", Some("<S-CR>")),
            (SHIFT, Keysym::Left, "", Some("<S-Left>")),
            (SHIFT, Keysym::space, " ", Some("<S-Space>")),
        ]);
    }

    #[test]
    fn ctrl() {
        check(&[
            (CTRL, Keysym::a, "\x01", Some("<C-a>")),
            (CTRL, Keysym::backslash, "\x1c", Some("<C-\\>")),
            (CTRL, Keysym::bracketleft, "\x1b", Some("<C-[>")),
            (CTRL, Keysym::_1, "1", Some("<C-1>")),
            (CTRL, Keysym::less, "<", Some("<C-lt>")),
            (CTRL, Keysym::Return, "\r", Some("<C-CR>")),
            (CTRL, Keysym::Home, "", Some("<C-Home>")),
            // Ctrl ignores case
            (CTRL_SHIFT, Keysym::R, "\x12", Some("<C-r>")),
            (CTRL_SHIFT, Keysym::exclam, "!", Some("<C-!>")),
            (CTRL_SHIFT, Keysym::ISO_Left_Tab, "\t", Some("<C-S-Tab>")),
            (CTRL_SHIFT, Keysym::Left, "", Some("<C-S-Left>")),
        ]);
    }

    #[test]
    fn alt() {
        check(&[
            (ALT, Keysym::a, "a", Some("<A-a>")),
            (ALT, Keysym::_1, "1", Some("<A-1>")),
            (ALT, Keysym::semicolon, ";", Some("<A-;>")),
            (ALT, Keysym::less, "<", Some("<A-lt>")),
            (ALT, Keysym::kana_A, "ア", Some("<A-ア>")),
            (ALT, Keysym::Return, "\r", Some("<A-CR>")),
            (ALT, Keysym::Escape, "\x1b", Some("<A-Esc>")),
            (ALT, Keysym::F5, "", Some("<A-F5>")),
            // Alt keeps case
            (ALT_SHIFT, Keysym::A, "A", Some("<A-A>")),
            (ALT_SHIFT, Keysym::ISO_Left_Tab, "\t", Some("<A-S-Tab>")),
        ]);
    }

    #[test]
    fn ctrl_and_alt_together() {
        check(&[
            (CTRL_ALT, Keysym::a, "\x01", Some("<A-C-a>")),
            (CTRL_ALT, Keysym::backslash, "\x1c", Some("<A-C-\\>")),
            (CTRL_ALT, Keysym::Return, "\r", Some("<A-C-CR>")),
            (ALL, Keysym::ISO_Left_Tab, "\t", Some("<A-C-S-Tab>")),
            (ALL, Keysym::A, "\x01", Some("<A-C-a>")),
        ]);
    }

    #[test]
    fn super_is_dropped() {
        check(&[
            (LOGO, Keysym::a, "a", None),
            (LOGO, Keysym::Return, "\r", None),
        ]);
    }

    #[test]
    fn no_vim_key() {
        check(&[
            (NONE, Keysym::Shift_L, "", None),
            (NONE, Keysym::Control_L, "", None),
            (CTRL, Keysym::Control_L, "", None),
            (ALT, Keysym::Shift_L, "", None),
            (NONE, Keysym::Menu, "", None),
            (NONE, Keysym::XF86_AudioMute, "", None),
        ]);
    }

    // ── key_name ──

    #[test]
    fn key_name_strips_modifiers() {
        assert_eq!(key_name("<CR>"), Some("CR"));
        assert_eq!(key_name("<C-CR>"), Some("CR"));
        assert_eq!(key_name("<A-C-S-CR>"), Some("CR"));
        assert_eq!(key_name("<C-->"), Some("-"));
        assert_eq!(key_name("<C-lt>"), Some("lt"));
        assert_eq!(key_name("a"), None);
        assert_eq!(key_name("<lt>"), Some("lt"));
    }
}
