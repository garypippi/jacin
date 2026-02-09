use wayland_client::protocol::wl_keyboard;
use xkbcommon::xkb;

use crate::State;
use crate::neovim::{PendingState, pending_state};

/// Distinguishes physical key presses from repeat events
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOrigin {
    Physical,
    Repeat,
}

/// Convert an XKB keysym + modifiers to Vim notation.
///
/// Returns `None` if the key has no Vim representation (e.g. bare modifier keys).
pub fn keysym_to_vim(ctrl: bool, alt: bool, keysym: xkb::Keysym, utf8: &str) -> Option<String> {
    use xkbcommon::xkb::Keysym;

    // Get base key representation first
    let base_key = match keysym {
        Keysym::Return | Keysym::KP_Enter => Some("CR".to_string()),
        Keysym::BackSpace => Some("BS".to_string()),
        Keysym::Tab => Some("Tab".to_string()),
        Keysym::Escape => Some("Esc".to_string()),
        Keysym::space => Some("Space".to_string()),
        Keysym::Left => Some("Left".to_string()),
        Keysym::Right => Some("Right".to_string()),
        Keysym::Up => Some("Up".to_string()),
        Keysym::Down => Some("Down".to_string()),
        _ if keysym.raw() >= Keysym::a.raw() && keysym.raw() <= Keysym::z.raw() => {
            // Lowercase letter
            let c = (keysym.raw() - Keysym::a.raw() + b'a' as u32) as u8 as char;
            Some(c.to_string())
        }
        _ => None,
    };

    // Handle Alt combinations
    if alt {
        if let Some(key) = base_key {
            return Some(format!("<A-{}>", key));
        }
        if !utf8.is_empty() && !utf8.chars().all(|c| c.is_control()) {
            let escaped = utf8.replace('<', "lt");
            return Some(format!("<A-{}>", escaped));
        }
        return None;
    }

    // Handle Ctrl combinations
    if ctrl {
        if let Some(key) = base_key {
            return Some(format!("<C-{}>", key));
        }
        return None;
    }

    // No modifier: wrap special keys in <>, return letters/printable as-is
    match keysym {
        Keysym::Return | Keysym::KP_Enter => Some("<CR>".to_string()),
        Keysym::BackSpace => Some("<BS>".to_string()),
        Keysym::Tab => Some("<Tab>".to_string()),
        Keysym::Escape => Some("<Esc>".to_string()),
        Keysym::space => Some("<Space>".to_string()),
        Keysym::Left => Some("<Left>".to_string()),
        Keysym::Right => Some("<Right>".to_string()),
        Keysym::Up => Some("<Up>".to_string()),
        Keysym::Down => Some("<Down>".to_string()),
        _ => {
            // Printable characters
            if !utf8.is_empty() && !utf8.chars().all(|c| c.is_control()) {
                // Escape '<' as '<lt>' for nvim_input (bare '<' starts a key sequence)
                Some(utf8.replace('<', "<lt>"))
            } else {
                None
            }
        }
    }
}

impl State {
    pub(crate) fn handle_key(
        &mut self,
        key: u32,
        key_state: wl_keyboard::KeyState,
        _origin: KeyOrigin,
    ) {
        let state_str = match key_state {
            wl_keyboard::KeyState::Pressed => "pressed",
            wl_keyboard::KeyState::Released => "released",
            _ => "unknown",
        };
        log::debug!(
            "[KEY] code={}, state={}, ctrl={}",
            key,
            state_str,
            self.keyboard.ctrl_pressed
        );

        // Handle key releases
        if key_state != wl_keyboard::KeyState::Pressed {
            self.keyboard.handle_key_release(key);
            return;
        }

        // Check if key should be ignored
        if self.keyboard.should_ignore_key(key) {
            log::debug!("[KEY] Ignoring key {}", key);
            return;
        }

        // Get keysym and UTF-8
        let Some((keysym, utf8)) = self.keyboard.get_key_info(key) else {
            log::warn!("No xkb state, cannot process key");
            return;
        };
        log::debug!("[KEY] keysym={:?}, utf8={:?}", keysym, utf8);

        // Convert key to Vim notation and send to Neovim
        let vim_key = keysym_to_vim(
            self.keyboard.ctrl_pressed,
            self.keyboard.alt_pressed,
            keysym,
            &utf8,
        );
        log::debug!("[KEY] vim_key={:?}", vim_key);

        if let Some(ref vim_key) = vim_key {
            // Track state before sending to Neovim
            let was_normal = self.keypress.is_normal_mode();
            let was_visual = self.keypress.is_visual_mode();
            let before = pending_state().load();
            let was_motion_pending = before.is_motion();
            let was_register_pending = before.is_register();
            let was_insert_register_pending = before == PendingState::InsertRegister;

            // Store raw keycode for potential passthrough
            self.current_keycode = Some(key);

            self.send_to_nvim(vim_key);
            // Wait for Neovim response with timeout
            self.wait_for_nvim_response();

            // Clear keycode after processing
            self.current_keycode = None;

            // Check state after Neovim response
            let after = pending_state().load();
            let now_pending = after.is_pending();
            let is_normal = self.keypress.is_normal_mode();
            let is_visual = self.keypress.is_visual_mode();
            let is_insert = self.keypress.vim_mode == "i";

            // Command-line mode: display updates come via CmdlineUpdate messages
            if after == PendingState::CommandLine {
                return;
            }

            if now_pending {
                // In pending state (operator or register) - accumulate key and show
                self.keypress.push_key(vim_key);
                self.update_keypress_from_pending();
                self.show_keypress();
            } else if was_insert_register_pending && is_insert {
                // Just completed <C-r> + register in insert mode - show full sequence
                self.keypress.push_key(vim_key);
                self.show_keypress();
            } else if (was_normal || was_visual) && is_insert {
                // Just entered insert mode from normal/visual - show entry key (i, a, A, c, etc.)
                self.keypress.clear();
                self.keypress.push_key(vim_key);
                self.show_keypress();
            } else if was_normal && is_visual {
                // Entered visual mode from normal - show 'v'
                self.keypress.clear();
                self.keypress.push_key(vim_key);
                self.show_keypress();
            } else if is_normal && was_visual {
                // Visual operator completed (d, y, x from visual) - show key
                self.keypress.push_key(vim_key);
                self.show_keypress();
            } else if is_normal {
                // In normal mode - show completed sequences
                if was_motion_pending || was_register_pending {
                    // Sequence completed (e.g., "d$", "\"ay$") - add final key
                    self.keypress.push_key(vim_key);
                    self.show_keypress();
                }
                // Don't show standalone normal mode keys (h, j, k, l, etc.)
            } else if is_visual {
                // Visual mode movement - don't hide existing display
            } else {
                // In insert mode typing - hide keypress display
                self.hide_keypress();
            }
        } else if !utf8.is_empty() && !utf8.chars().all(|c| c.is_control()) {
            // Fallback: if no Neovim or no vim key, use local preedit
            if self.nvim.is_none() {
                self.ime.preedit.push_str(&utf8);
                log::debug!("[PREEDIT] buffer={:?}", self.ime.preedit);
                self.update_preedit();
            }
        } else {
            log::debug!(
                "[SKIP] no printable char, ctrl={}",
                self.keyboard.ctrl_pressed
            );
        }
    }

    pub(crate) fn send_to_nvim(&self, key: &str) {
        if let Some(ref nvim) = self.nvim {
            nvim.send_key(key);
        }
    }

    pub(crate) fn wait_for_nvim_response(&mut self) {
        if let Some(ref nvim) = self.nvim {
            // Block waiting for response with 200ms timeout
            if let Some(msg) = nvim.recv_timeout(std::time::Duration::from_millis(200)) {
                self.handle_nvim_message(msg);
            }
        }
    }

    pub(crate) fn update_modifiers(
        &mut self,
        mods_depressed: u32,
        mods_latched: u32,
        mods_locked: u32,
        group: u32,
    ) {
        let old_ctrl = self.keyboard.ctrl_pressed;
        let old_alt = self.keyboard.alt_pressed;

        self.keyboard
            .update_modifiers(mods_depressed, mods_latched, mods_locked, group);

        if old_ctrl != self.keyboard.ctrl_pressed {
            log::debug!(
                "[MOD] ctrl changed: {} -> {}",
                old_ctrl,
                self.keyboard.ctrl_pressed
            );
        }
        if old_alt != self.keyboard.alt_pressed {
            log::debug!(
                "[MOD] alt changed: {} -> {}",
                old_alt,
                self.keyboard.alt_pressed
            );
        }
    }

    pub(crate) fn show_keypress(&mut self) {
        self.update_popup();
    }

    pub(crate) fn hide_keypress(&mut self) {
        self.keypress.clear();
        self.keypress_timer_token = None;
        self.update_popup();
    }

    pub(crate) fn update_keypress_from_pending(&mut self) {
        // Sync keypress state with neovim pending state
        let state = pending_state().load();
        if state.is_pending() {
            self.keypress.set_pending(state);
        } else {
            self.hide_keypress();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::keysym_to_vim;
    use xkbcommon::xkb::Keysym;

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
    fn ctrl_modifier() {
        assert_eq!(
            keysym_to_vim(true, false, Keysym::a, "a"),
            Some("<C-a>".into())
        );
        assert_eq!(
            keysym_to_vim(true, false, Keysym::Return, ""),
            Some("<C-CR>".into())
        );
    }

    #[test]
    fn alt_modifier() {
        assert_eq!(
            keysym_to_vim(false, true, Keysym::a, "a"),
            Some("<A-a>".into())
        );
        assert_eq!(
            keysym_to_vim(false, true, Keysym::Return, ""),
            Some("<A-CR>".into())
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
        assert_eq!(keysym_to_vim(false, false, Keysym::Control_L, ""), None);
    }

    #[test]
    fn arrow_keys() {
        assert_eq!(
            keysym_to_vim(false, false, Keysym::Left, ""),
            Some("<Left>".into())
        );
        assert_eq!(
            keysym_to_vim(false, false, Keysym::Up, ""),
            Some("<Up>".into())
        );
    }

    #[test]
    fn japanese_utf8() {
        assert_eq!(
            keysym_to_vim(false, false, Keysym::NoSymbol, "あ"),
            Some("あ".into())
        );
    }
}
