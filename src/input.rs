use wayland_client::protocol::wl_keyboard;

use crate::State;
use crate::keysym::keysym_to_vim;
use crate::neovim::PendingState;

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

        if key_state != wl_keyboard::KeyState::Pressed {
            self.keyboard.handle_key_release(key);
            return;
        }

        if self.keyboard.should_ignore_key(key) {
            log::debug!("[KEY] Ignoring key {}", key);
            return;
        }

        let Some((keysym, utf8)) = self.keyboard.get_key_info(key) else {
            log::warn!("No xkb state, cannot process key");
            return;
        };
        log::debug!("[KEY] keysym={:?}, utf8={:?}", keysym, utf8);

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
            // Wait for Neovim response with timeout; on timeout keep the
            // previous pending state
            let after = self
                .wait_for_nvim_response()
                .unwrap_or(self.model.keypress.pending_type);

            self.current_keycode = None;

            // Command-line mode: display updates come via ext_cmdline (cmdline_show)
            if after == PendingState::CommandLine {
                return;
            }

            // Keypress display: in insert mode, only show Ctrl/Alt modified keys
            // (e.g., <C-r>a, <C-w>) and pending register names (key after <C-r>);
            // suppress normal typing, <BS>, <CR>, etc.
            let should_show_keypress = !self.model.view.vim_mode.starts_with('i')
                || self.keyboard.ctrl_pressed
                || self.keyboard.alt_pressed
                || self.model.keypress.pending_type == PendingState::InsertRegister;

            if should_show_keypress {
                self.model.keypress.push_key(vim_key);
                self.update_popup();
            }

            self.model.keypress.set_pending(after);
        } else {
            log::debug!(
                "[SKIP] no printable char, ctrl={}",
                self.keyboard.ctrl_pressed
            );
        }
        _perf.mode = self.model.view.vim_mode.clone();
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

    /// Handle Neovim messages until `KeyProcessed` arrives (or 200ms deadline).
    /// Returns the pending state reported by `KeyProcessed`, None on timeout.
    pub(crate) fn wait_for_nvim_response(&mut self) -> Option<PendingState> {
        use crate::neovim::FromNeovim;

        let _perf = PerfGuard::new("nvim_rpc");

        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                log::debug!("[NVIM] wait_for_nvim_response: deadline reached");
                return None;
            }
            let msg = self.nvim.as_ref().and_then(|n| n.recv_timeout(remaining));
            match msg {
                Some(FromNeovim::KeyProcessed { pending }) => return Some(pending),
                Some(msg) => self.handle_nvim_message(msg),
                None => return None,
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
