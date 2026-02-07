use wayland_client::protocol::wl_keyboard;
use xkbcommon::xkb;

use crate::neovim::{PendingState, pending_state};
use crate::State;

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
            return Some(format!("<A-{}>", utf8));
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
                Some(utf8.to_string())
            } else {
                None
            }
        }
    }
}

impl State {
    pub(crate) fn handle_key(&mut self, key: u32, key_state: wl_keyboard::KeyState, _origin: KeyOrigin) {
        let state_str = match key_state {
            wl_keyboard::KeyState::Pressed => "pressed",
            wl_keyboard::KeyState::Released => "released",
            _ => "unknown",
        };
        eprintln!(
            "[KEY] code={}, state={}, ctrl={}",
            key, state_str, self.keyboard.ctrl_pressed
        );

        // Handle key releases
        if key_state != wl_keyboard::KeyState::Pressed {
            self.keyboard.handle_key_release(key);
            return;
        }

        // Check if key should be ignored
        if self.keyboard.should_ignore_key(key) {
            eprintln!("[KEY] Ignoring key {}", key);
            return;
        }

        // Get keysym and UTF-8
        let Some((keysym, utf8)) = self.keyboard.get_key_info(key) else {
            eprintln!("No xkb state, cannot process key");
            return;
        };
        eprintln!("[KEY] keysym={:?}, utf8={:?}", keysym, utf8);

        // Handle Ctrl+C to exit
        use xkbcommon::xkb::Keysym;
        if self.keyboard.ctrl_pressed && keysym == Keysym::c {
            eprintln!("\nCtrl+C pressed, releasing keyboard and exiting...");
            self.wayland.release_keyboard();
            self.pending_exit = true;
            return;
        }

        // Convert key to Vim notation and send to Neovim
        let vim_key = keysym_to_vim(
            self.keyboard.ctrl_pressed,
            self.keyboard.alt_pressed,
            keysym,
            &utf8,
        );
        eprintln!("[KEY] vim_key={:?}", vim_key);

        if let Some(ref vim_key) = vim_key {
            // Track state before sending to Neovim
            let was_normal = self.keypress.is_normal_mode();
            let before = pending_state().load();
            let was_motion_pending = before.is_motion();
            let was_register_pending = before.is_register();
            let was_insert_register_pending = before == PendingState::InsertRegister;

            self.send_to_nvim(vim_key);
            // Wait for Neovim response with timeout
            self.wait_for_nvim_response();

            // Check state after Neovim response
            let after = pending_state().load();
            let now_pending = after.is_pending();
            let is_normal = self.keypress.is_normal_mode();
            let is_insert = self.keypress.vim_mode == "i";

            if now_pending {
                // In pending state (operator or register) - accumulate key and show
                self.keypress.push_key(vim_key);
                self.update_keypress_from_pending();
                self.show_keypress();
            } else if was_insert_register_pending && is_insert {
                // Just completed <C-r> + register in insert mode - show full sequence
                self.keypress.push_key(vim_key);
                self.show_keypress();
            } else if was_normal && is_insert {
                // Just entered insert mode from normal - show the entry key (i, a, A, o, etc.)
                self.keypress.clear();
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
            } else {
                // In insert mode typing - hide keypress display
                self.hide_keypress();
            }
        } else if !utf8.is_empty() && !utf8.chars().all(|c| c.is_control()) {
            // Fallback: if no Neovim or no vim key, use local preedit
            if self.nvim.is_none() {
                self.ime.preedit.push_str(&utf8);
                eprintln!("[PREEDIT] buffer={:?}", self.ime.preedit);
                self.update_preedit();
            }
        } else {
            eprintln!(
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
            eprintln!(
                "[MOD] ctrl changed: {} -> {}",
                old_ctrl, self.keyboard.ctrl_pressed
            );
        }
        if old_alt != self.keyboard.alt_pressed {
            eprintln!(
                "[MOD] alt changed: {} -> {}",
                old_alt, self.keyboard.alt_pressed
            );
        }
    }

    pub(crate) fn show_keypress(&mut self) {
        self.update_popup();
    }

    pub(crate) fn hide_keypress(&mut self) {
        self.keypress.clear();
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
