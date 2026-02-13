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
            // Drain stale messages before setting current_keycode to avoid
            // stale PassthroughKey using the new key's keycode
            self.drain_stale_nvim_messages();

            // Store raw keycode for potential passthrough
            self.current_keycode = Some(key);

            self.send_to_nvim(vim_key);
            // Wait for Neovim response with timeout
            self.wait_for_nvim_response();

            // Clear keycode after processing
            self.current_keycode = None;

            // Check state after Neovim response
            let after = pending_state().load();

            // Command-line mode: display updates come via CmdlineUpdate messages
            if after == PendingState::CommandLine {
                return;
            }

            // Keypress display: show everything except insert-mode printable typing
            let is_insert_printable_typing = self.keypress.vim_mode == "i"
                && !self.keyboard.ctrl_pressed
                && !self.keyboard.alt_pressed
                && is_printable(&utf8);

            if !is_insert_printable_typing {
                self.keypress.push_key(vim_key);
                self.update_popup();
            }

            if after.is_pending() {
                self.keypress.set_pending(after);
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

    fn drain_stale_nvim_messages(&mut self) {
        loop {
            let msg = self.nvim.as_ref().and_then(|n| n.try_recv());
            match msg {
                Some(stale) => {
                    log::debug!("[NVIM] Draining stale message: {:?}", stale);
                    self.handle_nvim_message(stale);
                }
                None => break,
            }
        }
    }

    pub(crate) fn wait_for_nvim_response(&mut self) {
        use crate::neovim::FromNeovim;

        let _perf = PerfGuard::new("nvim_rpc");

        // Loop until KeyProcessed or deadline (200ms)
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                log::debug!("[NVIM] wait_for_nvim_response: deadline reached");
                break;
            }
            let msg = self.nvim.as_ref().and_then(|n| n.recv_timeout(remaining));
            match msg {
                Some(msg) => {
                    let is_key_processed = matches!(msg, FromNeovim::KeyProcessed);
                    self.handle_nvim_message(msg);
                    if is_key_processed {
                        break;
                    }
                }
                None => break,
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
}
