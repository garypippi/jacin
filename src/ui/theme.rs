//! Popup colors from Neovim's highlight groups
//!
//! The popup follows the colorscheme: Normal (`default_colors_set`) for the
//! background and plain text, and builtin UI groups (`hl_group_set`) for the
//! rest. Colors Neovim doesn't set fall back to the built-in theme.

use super::layout::Rgba;
use crate::neovim::HlAttr;
use crate::state::Screen;

/// Foreground and background of a highlight
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pair {
    pub fg: Rgba,
    pub bg: Rgba,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// Popup background (Normal), with the configured opacity
    pub bg: Rgba,
    /// Text without a highlight (Normal)
    pub fg: Rgba,
    /// Border, separator and the insert-mode strip (FloatBorder, else the
    /// dim NonText: WinSeparator is often as bright as the text)
    pub border: Rgba,
    /// Messages, command line and showcmd (MsgArea)
    pub msg: Rgba,
    /// Candidate rows (Pmenu, PmenuSel)
    pub pmenu: Pair,
    pub pmenu_sel: Pair,
    /// Candidate scrollbar (PmenuSbar, PmenuThumb backgrounds)
    pub scrollbar: Rgba,
    pub thumb: Rgba,
    /// Block cursor (Cursor); None: the cell's colors reversed
    pub cursor: Option<Pair>,
    /// Recording indicator (ErrorMsg foreground)
    pub recording: Rgba,
}

impl Default for Theme {
    fn default() -> Self {
        let bg = (40, 44, 52, 255);
        let fg = (220, 223, 228, 255);
        Self {
            bg,
            fg,
            border: (80, 84, 92, 255),
            msg: fg,
            pmenu: Pair { fg, bg },
            pmenu_sel: Pair {
                fg,
                bg: (61, 89, 161, 255),
            },
            scrollbar: (60, 64, 72, 255),
            thumb: (100, 104, 112, 255),
            cursor: Some(Pair {
                fg: bg,
                bg: (97, 175, 239, 255),
            }),
            recording: (224, 108, 117, 255),
        }
    }
}

impl Theme {
    /// `opacity` (0.0–1.0) applies to the popup background only
    pub fn from_screen(screen: &Screen, opacity: f32) -> Self {
        let fallback = Self::default();
        let normal = Pair {
            fg: screen.default_colors.fg.map_or(fallback.fg, rgb),
            bg: screen.default_colors.bg.map_or(fallback.bg, rgb),
        };
        let group = |name: &str| screen.hl_group(name);
        let colors = |name: &str| group(name).map(|attr| resolve(attr, normal));
        let explicit_fg = |name: &str| group(name).and_then(|attr| attr.foreground).map(rgb);

        let alpha = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
        Self {
            bg: (normal.bg.0, normal.bg.1, normal.bg.2, alpha),
            fg: normal.fg,
            border: explicit_fg("FloatBorder")
                .or_else(|| explicit_fg("NonText"))
                .unwrap_or(fallback.border),
            msg: colors("MsgArea").map_or(normal.fg, |c| c.fg),
            pmenu: colors("Pmenu").unwrap_or(fallback.pmenu),
            pmenu_sel: colors("PmenuSel").unwrap_or(fallback.pmenu_sel),
            scrollbar: colors("PmenuSbar").map_or(fallback.scrollbar, |c| c.bg),
            thumb: colors("PmenuThumb").map_or(fallback.thumb, |c| c.bg),
            cursor: match group("Cursor") {
                Some(attr) if attr.foreground.is_some() && attr.background.is_some() => {
                    Some(resolve(attr, normal))
                }
                Some(_) => None,
                None => fallback.cursor,
            },
            recording: explicit_fg("ErrorMsg").unwrap_or(fallback.recording),
        }
    }
}

/// Colors of a highlight over Normal
fn resolve(attr: &HlAttr, normal: Pair) -> Pair {
    let fg = attr.foreground.map_or(normal.fg, rgb);
    let bg = attr.background.map_or(normal.bg, rgb);
    if attr.reverse {
        Pair { fg: bg, bg: fg }
    } else {
        Pair { fg, bg }
    }
}

/// 0xRRGGBB → opaque color
pub(crate) fn rgb(rgb: u32) -> Rgba {
    ((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::neovim::GridEvent;

    fn screen(groups: &[(&str, HlAttr)]) -> Screen {
        let mut screen = Screen::default();
        screen.apply(GridEvent::DefaultColors {
            fg: Some(0x111111),
            bg: Some(0xeeeeee),
            sp: None,
        });
        for (id, (name, attr)) in groups.iter().enumerate() {
            let id = id as u64 + 1;
            screen.apply(GridEvent::HlAttrDefine {
                id,
                attr: attr.clone(),
            });
            screen.apply(GridEvent::HlGroupSet {
                name: name.to_string(),
                id,
            });
        }
        screen
    }

    fn attr(fg: Option<u32>, bg: Option<u32>) -> HlAttr {
        HlAttr {
            foreground: fg,
            background: bg,
            ..Default::default()
        }
    }

    #[test]
    fn follows_normal_and_groups() {
        let theme = Theme::from_screen(
            &screen(&[
                ("Pmenu", attr(None, Some(0xdddddd))),
                ("PmenuSel", attr(Some(0xffffff), Some(0x0000ff))),
                ("MsgArea", attr(Some(0x222222), None)),
                ("WinSeparator", attr(Some(0x111111), None)),
                ("NonText", attr(Some(0x999999), None)),
                ("ErrorMsg", attr(Some(0xff0000), None)),
            ]),
            1.0,
        );
        assert_eq!(theme.bg, (0xee, 0xee, 0xee, 255));
        assert_eq!(theme.fg, (0x11, 0x11, 0x11, 255));
        // Unset foreground falls back to Normal
        assert_eq!(
            theme.pmenu,
            Pair {
                fg: (0x11, 0x11, 0x11, 255),
                bg: (0xdd, 0xdd, 0xdd, 255)
            }
        );
        assert_eq!(theme.pmenu_sel.bg, (0, 0, 0xff, 255));
        assert_eq!(theme.msg, (0x22, 0x22, 0x22, 255));
        // FloatBorder has no color of its own: NonText
        assert_eq!(theme.border, (0x99, 0x99, 0x99, 255));
        assert_eq!(theme.recording, (0xff, 0, 0, 255));
    }

    #[test]
    fn opacity_applies_to_the_background() {
        let theme = Theme::from_screen(&screen(&[]), 0.5);
        assert_eq!(theme.bg.3, 128);
        assert_eq!(theme.fg.3, 255);
        assert_eq!(Theme::from_screen(&screen(&[]), 2.0).bg.3, 255);
    }

    #[test]
    fn cursor_without_colors_reverses_the_cell() {
        let reverse = HlAttr {
            reverse: true,
            ..Default::default()
        };
        let theme = Theme::from_screen(&screen(&[("Cursor", reverse)]), 1.0);
        assert_eq!(theme.cursor, None);

        let colored = attr(Some(0x000000), Some(0x00ff00));
        let theme = Theme::from_screen(&screen(&[("Cursor", colored)]), 1.0);
        assert_eq!(theme.cursor.unwrap().bg, (0, 0xff, 0, 255));
    }

    #[test]
    fn nothing_from_neovim_keeps_the_builtin_theme() {
        assert_eq!(
            Theme::from_screen(&Screen::default(), 1.0),
            Theme::default()
        );
    }
}
