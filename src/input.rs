use wayland_client::protocol::wl_keyboard;

use crate::State;
use crate::keysym::keysym_to_vim;
use crate::neovim::MAX_QUEUED_KEYS;
use crate::state::SentKey;

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
        log::debug!("[KEY] code={key}, state={state_str}");

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

        let mods = self.keyboard.key_mods();
        let vim_key = keysym_to_vim(mods, keysym, &utf8);
        log::debug!("[KEY] vim_key={vim_key:?}, {mods:?}");

        let Some(vim_key) = vim_key else {
            log::debug!("[SKIP] no Vim key");
            return;
        };
        let mode = self.model.view.mode;
        let Some(nvim) = self.nvim.as_mut() else {
            return;
        };
        if nvim.queued_keys() >= MAX_QUEUED_KEYS {
            log::warn!("[KEY] {vim_key:?} dropped: Neovim is not keeping up");
            return;
        }
        // Kept with the key to show it and in case Neovim passes it through
        let seq = self.model.sent_keys.push(SentKey {
            keycode: key,
            mods: self.keyboard.mods,
            vim_key: vim_key.clone(),
        });
        // Not waited for: KeyProcessed and the rest arrive as Neovim messages
        let events = nvim.push_key(seq, &vim_key, mode);
        for event in events {
            self.handle_nvim_message(event);
        }
        _perf.mode = format!("{:?}", self.model.view.mode);
    }

    /// A held key repeats only while Neovim keeps up: repeats are not queued
    /// behind keys still being processed
    pub(crate) fn handle_repeat_key(&mut self, key: u32) {
        if self.nvim.as_ref().is_some_and(|nvim| nvim.is_busy()) {
            return;
        }
        self.handle_key(key, wl_keyboard::KeyState::Pressed);
    }

    pub(crate) fn update_modifiers(
        &mut self,
        mods_depressed: u32,
        mods_latched: u32,
        mods_locked: u32,
        group: u32,
    ) {
        let old = self.keyboard.key_mods();
        self.keyboard
            .update_modifiers(mods_depressed, mods_latched, mods_locked, group);
        let new = self.keyboard.key_mods();
        if old != new {
            log::debug!("[MOD] {old:?} -> {new:?}");
        }
    }
}
