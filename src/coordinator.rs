use std::sync::atomic::Ordering;

use crate::State;
use crate::model::Effect;
use crate::neovim::{self, FromNeovim};
use crate::ui::{MAX_GRID_ROWS, PopupContent};

impl State {
    /// Common cleanup shared by toggle-off and deactivate:
    /// clear all model display state and release IME resources.
    pub(crate) fn reset_ime_state(&mut self) {
        self.model.reset();
        self.release_ime_resources();
    }

    /// Cancel timers, hide popup, release keyboard grab (non-model cleanup).
    fn release_ime_resources(&mut self) {
        self.repeat.cancel();
        self.repeat_timer_token = None;
        self.keypress_timer_token = None;
        self.hide_popup();
        self.wayland.release_keyboard();
        self.keyboard.reset_modifiers();
    }

    pub(crate) fn handle_ime_toggle(&mut self) {
        let was_enabled = self.model.ime.is_enabled();
        log::info!("[IME] Toggle: was_enabled = {}", was_enabled);

        if !was_enabled {
            // Respawn Neovim if it exited (e.g., after :q)
            if self.nvim.is_none() {
                match neovim::spawn_neovim(self.config.clone(), Some(self.nvim_wake.clone())) {
                    Ok(handle) => {
                        log::info!("[IME] Respawned Neovim backend");
                        if let Some((cols, rows)) = self.ui_grid_size {
                            handle.resize_ui(cols, rows);
                        }
                        self.nvim = Some(handle);
                    }
                    Err(e) => {
                        log::error!("[IME] Failed to respawn Neovim: {}", e);
                        return;
                    }
                }
            }
            if self.wayland.active && self.wayland.keyboard_grab.is_none() {
                log::debug!("[IME] Grabbing keyboard");
                self.wayland.grab_keyboard();
                self.keyboard.pending_keymap = true;
                self.model.ime.start_enabling();
            }
        } else {
            // Disable IME - commit the buffer BEFORE releasing keyboard
            // (must match Commit handler order: commit first, then release)
            log::debug!("[IME] Releasing keyboard");
            // Whole buffer from the line-event mirror (no RPC: Neovim may be
            // blocked in getchar)
            if !self.model.view.buffer.is_empty() {
                self.wayland.commit_string(&self.model.view.buffer.text());
            }
            self.reset_ime_state();
            // Clear Neovim buffer (must clear here, not rely on Deactivate —
            // rapid re-enable can happen before Deactivate fires)
            if let Some(ref nvim) = self.nvim {
                nvim.send_key("<Esc>ggdG");
            }
            self.model.ime.disable();
        }
    }

    pub(crate) fn handle_nvim_message(&mut self, msg: FromNeovim) {
        match &msg {
            FromNeovim::Ready => log::info!("[NVIM] Backend ready!"),
            FromNeovim::NvimExited => log::info!("[NVIM] Neovim exited, disabling IME"),
            // Full cell dumps are too noisy even for debug
            FromNeovim::GridFlush(events) => {
                log::trace!("[NVIM] GridFlush ({} events)", events.len())
            }
            _ => log::debug!("[NVIM] {:?}", msg),
        }
        let effects = self.model.reduce(msg);
        self.apply_effects(effects);
    }

    /// Execute side effects requested by `Model::reduce`.
    fn apply_effects(&mut self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Render => self.update_popup(),
                Effect::CommitString(text) => self.wayland.commit_string(&text),
                Effect::DeleteSurrounding { before, after } => {
                    self.wayland.delete_surrounding(before, after);
                }
                Effect::PassthroughKey => self.passthrough_current_key(),
                Effect::NvimInput(keys) => {
                    if let Some(ref nvim) = self.nvim {
                        nvim.send_key(keys);
                    }
                }
                Effect::CancelToggle => self.toggle_flag.store(false, Ordering::SeqCst),
                Effect::NvimExited => {
                    self.release_ime_resources();
                    self.nvim = None;
                }
            }
        }
    }

    /// Send the key being processed through the virtual keyboard to the focused app
    fn passthrough_current_key(&mut self) {
        if let Some(keycode) = self.current_keycode {
            self.wayland.send_virtual_key(
                keycode,
                self.keyboard.mods_depressed,
                self.keyboard.mods_latched,
                self.keyboard.mods_locked,
                self.keyboard.mods_group,
            );
        } else {
            log::warn!("[IME] PassthroughKey but no current_keycode");
        }
    }

    /// Request a popup redraw. Rendering is deferred to the end of the
    /// event loop iteration (`flush_popup`), so the several messages one
    /// key produces (grid flush, mode change, candidates) render only once.
    pub(crate) fn update_popup(&mut self) {
        self.popup_dirty = true;
    }

    /// Render the popup if a redraw was requested since the last flush
    pub(crate) fn flush_popup(&mut self) {
        if std::mem::take(&mut self.popup_dirty) {
            self.render_popup();
        }
    }

    fn render_popup(&mut self) {
        // IME disabled: skip content generation entirely and ensure popup is hidden.
        // After toggle-off, Neovim sends a burst of push notifications (<Esc>ggdG
        // triggers mode changes and redraws) — without this guard, each notification
        // would rebuild PopupContent and potentially recreate/destroy surfaces.
        if !self.model.ime.is_enabled() {
            self.hide_popup();
            return;
        }
        let t = std::time::Instant::now();
        let content = PopupContent {
            vim_mode: self.model.view.vim_mode.clone(),
            keypress_entries: if let Some(ref cmdline) = self.model.view.cmdline {
                vec![cmdline.text.clone()]
            } else if self.model.keypress.should_show() {
                self.model
                    .keypress
                    .entries()
                    .iter()
                    .map(|e| e.text.clone())
                    .collect()
            } else {
                Vec::new()
            },
            candidates: self.model.view.candidates.clone(),
            selected: self.model.view.selected_candidate,
            transient_message: if self.model.view.candidates.is_empty() {
                self.model.view.transient_message.clone()
            } else {
                None
            },
            ime_enabled: self.model.ime.is_enabled(),
            recording: self.model.view.recording.clone(),
            rec_blink_on: self.animations.rec_blink.on,
            cmdline_cursor_pos: self.model.view.cmdline.as_ref().map(|c| c.cursor_byte),
            window_view: {
                let view = self.model.view.screen.window_view(MAX_GRID_ROWS);
                match &view {
                    Some(v) => log::debug!(
                        "[POPUP] window grid: {} rows, cursor={:?} (visible={})",
                        v.rows.len(),
                        v.cursor,
                        v.cursor_visible
                    ),
                    None => log::debug!("[POPUP] no window grid yet"),
                }
                view
            },
        };
        if let Some(ref mut popup) = self.popup {
            let qh = self.wayland.qh.clone();
            popup.update(&content, &qh);
        }
        log::trace!(
            "[PERF] render_popup: {:.2}ms",
            t.elapsed().as_secs_f64() * 1000.0
        );
    }

    pub(crate) fn hide_popup(&mut self) {
        if let Some(ref mut popup) = self.popup {
            popup.hide();
        }
    }
}
