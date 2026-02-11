use wayland_client::protocol::wl_keyboard;

use crate::State;
use crate::keysym::{is_printable, keysym_to_vim};
use crate::neovim::{PendingState, pending_state};

/// Scope guard that logs elapsed time on drop.
struct PerfGuard {
    name: &'static str,
    mode: String,
    start: std::time::Instant,
}

impl PerfGuard {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            mode: String::new(),
            start: std::time::Instant::now(),
        }
    }
}

impl Drop for PerfGuard {
    fn drop(&mut self) {
        let ms = self.start.elapsed().as_secs_f64() * 1000.0;
        if self.mode.is_empty() {
            log::trace!("[PERF] {}: {:.2}ms", self.name, ms);
        } else {
            log::trace!("[PERF] {}: {:.2}ms (mode={})", self.name, ms, self.mode);
        }
    }
}

impl State {
    pub(crate) fn handle_key(&mut self, key: u32, key_state: wl_keyboard::KeyState) {
        let mut _perf = PerfGuard::new("handle_key");
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
        } else if is_printable(&utf8) {
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
        _perf.mode = self.keypress.vim_mode.clone();
    }

    pub(crate) fn send_to_nvim(&self, key: &str) {
        if let Some(ref nvim) = self.nvim {
            nvim.send_key(key);
        }
    }

    pub(crate) fn wait_for_nvim_response(&mut self) {
        let _perf = PerfGuard::new("nvim_rpc");
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
