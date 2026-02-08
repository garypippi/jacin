use std::sync::atomic::Ordering;

use crate::neovim::{self, FromNeovim};
use crate::ui::PopupContent;
use crate::State;

impl State {
    pub(crate) fn handle_ime_toggle(&mut self) {
        let was_enabled = self.ime.is_enabled();
        log::info!("[IME] Toggle: was_enabled = {}", was_enabled);
        self.reactivation_count = 0;

        if !was_enabled {
            // Respawn Neovim if it exited (e.g., after :q)
            if self.nvim.is_none() {
                match neovim::spawn_neovim(self.config.clone()) {
                    Ok(handle) => {
                        log::info!("[IME] Respawned Neovim backend");
                        self.nvim = Some(handle);
                    }
                    Err(e) => {
                        log::error!("[IME] Failed to respawn Neovim: {}", e);
                        return;
                    }
                }
            }
            // Enable IME - grab keyboard
            if self.wayland.active && self.wayland.keyboard_grab.is_none() {
                log::debug!("[IME] Grabbing keyboard");
                self.wayland.grab_keyboard();
                self.keyboard.pending_keymap = true;
                self.ime.start_enabling();
            }
        } else {
            // Disable IME - commit preedit text, release keyboard, disable skkeleton
            log::debug!("[IME] Releasing keyboard");
            // Cancel any active key repeat
            self.repeat.cancel();
            // Commit any pending preedit text BEFORE releasing keyboard
            // (must match Commit handler order: commit first, then release)
            if !self.ime.preedit.is_empty() {
                self.wayland.commit_string(&self.ime.preedit);
            }
            self.wayland.release_keyboard();
            self.keyboard.reset_modifiers();
            // Clear Neovim buffer.
            // Must clear here rather than relying on Deactivate handler,
            // because rapid re-enable can happen before Deactivate fires.
            if let Some(ref nvim) = self.nvim {
                nvim.send_key("<Esc>ggdG");
            }
            // Clear preedit and keypress display
            self.ime.clear_preedit();
            self.keypress.clear();
            self.hide_popup();
            self.ime.disable();
        }
    }

    pub(crate) fn handle_nvim_message(&mut self, msg: FromNeovim) {
        match msg {
            FromNeovim::Ready => {
                log::info!("[NVIM] Backend ready!");
            }
            FromNeovim::Preedit(info) => {
                log::debug!(
                    "[NVIM] Preedit: {:?}, cursor: {}..{}, mode: {}",
                    info.text, info.cursor_begin, info.cursor_end, info.mode
                );
                self.ime.set_preedit(info.text, info.cursor_begin, info.cursor_end);
                self.keypress.set_vim_mode(&info.mode);
                self.update_preedit();
            }
            FromNeovim::Commit(text) => {
                log::debug!("[NVIM] Commit: {:?}", text);
                self.ime.clear_preedit();
                self.ime.clear_candidates();
                self.wayland.commit_string(&text);
                self.keypress.clear();
                // Consume any pending toggle (e.g., Alt in commit key <A-;> also
                // triggers SIGUSR1 toggle — don't let it re-enable after commit)
                self.toggle_flag.store(false, Ordering::SeqCst);
                // Clear Neovim buffer and stay in insert mode for next input
                if let Some(ref nvim) = self.nvim {
                    nvim.send_key("<Esc>ggdGi");
                }
                // Keep IME enabled — show icon-only popup
                self.update_popup();
            }
            FromNeovim::DeleteSurrounding { before, after } => {
                log::debug!(
                    "[NVIM] DeleteSurrounding: before={}, after={}",
                    before, after
                );
                self.wayland.delete_surrounding(before, after);
            }
            FromNeovim::Candidates(info) => {
                log::debug!("[NVIM] Candidates: {:?}, selected={}", info.candidates, info.selected);
                if info.candidates.is_empty() {
                    self.hide_candidates();
                } else {
                    self.ime.set_candidates(info.candidates, info.selected);
                    self.show_candidates();
                }
            }
            FromNeovim::VisualRange(selection) => {
                log::debug!("[NVIM] VisualRange: {:?}", selection);
                self.visual_display = selection;
                self.update_popup();
            }
            FromNeovim::KeyProcessed => {
                // Acknowledgment only — unblocks wait_for_nvim_response
            }
            FromNeovim::CmdlineUpdate(text) => {
                log::debug!("[NVIM] CmdlineUpdate: {:?}", text);
                self.keypress.accumulated = text;
                self.keypress.visible = true;
                self.keypress.set_vim_mode("c");
                self.update_popup();
            }
            FromNeovim::CmdlineCancelled => {
                log::debug!("[NVIM] CmdlineCancelled");
                self.keypress.clear();
                self.update_popup();
            }
            FromNeovim::NvimExited => {
                log::info!("[NVIM] Neovim exited, disabling IME");
                self.repeat.cancel();
                self.wayland.set_preedit("", 0, 0);
                self.ime.clear_preedit();
                self.ime.clear_candidates();
                self.keypress.clear();
                self.hide_popup();
                self.wayland.release_keyboard();
                self.keyboard.reset_modifiers();
                self.ime.disable();
                self.nvim = None;
            }
        }
    }

    pub(crate) fn update_preedit(&mut self) {
        let cursor_begin = self.ime.cursor_begin as i32;
        let cursor_end = self.ime.cursor_end as i32;
        // Don't send preedit to compositor when IME is disabled or deactivated.
        // Also skip empty preedit during re-activation (reactivation_count > 0) to avoid
        // sending commit(serial) that triggers compositor to cycle Deactivate/Activate again.
        if self.wayland.active && self.ime.is_enabled()
            && !(self.ime.preedit.is_empty() && self.reactivation_count > 0)
        {
            self.wayland
                .set_preedit(&self.ime.preedit, cursor_begin, cursor_end);
            log::debug!(
                "[PREEDIT] updated: {:?}, cursor: {}..{}",
                self.ime.preedit, cursor_begin, cursor_end
            );
        } else {
            log::debug!(
                "[PREEDIT] skipped (active={}, enabled={}): {:?}",
                self.wayland.active,
                self.ime.is_enabled(),
                self.ime.preedit
            );
        }
        // Show preedit window with cursor visualization
        self.show_preedit_window();
    }

    /// Update the unified popup with current state
    pub(crate) fn update_popup(&mut self) {
        let content = PopupContent {
            preedit: self.ime.preedit.clone(),
            cursor_begin: self.ime.cursor_begin,
            cursor_end: self.ime.cursor_end,
            vim_mode: self.keypress.vim_mode.clone(),
            keypress: if self.keypress.should_show() {
                self.keypress.accumulated.clone()
            } else {
                String::new()
            },
            candidates: self.ime.candidates.clone(),
            selected: self.ime.selected_candidate,
            visual_selection: self.visual_display.clone(),
            ime_enabled: self.ime.is_enabled(),
        };
        if let Some(ref mut popup) = self.popup {
            let qh = self.wayland.qh.clone();
            popup.update(&content, &qh);
        }
    }

    /// Hide the unified popup
    pub(crate) fn hide_popup(&mut self) {
        if let Some(ref mut popup) = self.popup {
            popup.hide();
        }
    }

    pub(crate) fn show_candidates(&mut self) {
        self.update_popup();
    }

    pub(crate) fn hide_candidates(&mut self) {
        self.ime.clear_candidates();
        self.update_popup();
    }

    pub(crate) fn show_preedit_window(&mut self) {
        self.update_popup();
    }
}
