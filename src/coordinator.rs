use std::sync::atomic::Ordering;

use crate::State;
use crate::neovim::{self, FromNeovim};
use crate::ui::PopupContent;

impl State {
    /// Common cleanup shared by toggle-off, deactivate, and NvimExited:
    /// cancel timers, clear all display state, release keyboard grab.
    pub(crate) fn reset_ime_state(&mut self) {
        self.repeat.cancel();
        self.repeat_timer_token = None;
        self.ime.clear_preedit();
        self.ime.clear_candidates();
        self.keypress.clear();
        self.keypress_timer_token = None;
        self.keypress.recording.clear();
        self.visual_display = None;
        self.hide_popup();
        self.wayland.release_keyboard();
        self.keyboard.reset_modifiers();
    }

    pub(crate) fn handle_ime_toggle(&mut self) {
        let was_enabled = self.ime.is_enabled();
        log::info!("[IME] Toggle: was_enabled = {}", was_enabled);

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
            // Disable IME - commit preedit text BEFORE releasing keyboard
            // (must match Commit handler order: commit first, then release)
            log::debug!("[IME] Releasing keyboard");
            if !self.ime.preedit.is_empty() {
                self.wayland.commit_string(&self.ime.preedit);
            }
            self.reset_ime_state();
            // Clear Neovim buffer (must clear here, not rely on Deactivate —
            // rapid re-enable can happen before Deactivate fires)
            if let Some(ref nvim) = self.nvim {
                nvim.send_key("<Esc>ggdG");
            }
            self.ime.disable();
        }
    }

    pub(crate) fn handle_nvim_message(&mut self, msg: FromNeovim) {
        match msg {
            FromNeovim::Ready => {
                log::info!("[NVIM] Backend ready!");
            }
            FromNeovim::Preedit(info) => self.on_preedit(info),
            FromNeovim::Commit(text) => self.on_commit(text),
            FromNeovim::DeleteSurrounding { before, after } => {
                self.on_delete_surrounding(before, after);
            }
            FromNeovim::Candidates(info) => self.on_candidates(info),
            FromNeovim::VisualRange(selection) => self.on_visual_range(selection),
            FromNeovim::PassthroughKey => self.on_passthrough_key(),
            FromNeovim::KeyProcessed => {
                // Acknowledgment only — unblocks wait_for_nvim_response
            }
            FromNeovim::CmdlineUpdate(text) => self.on_cmdline_update(text),
            FromNeovim::CmdlineCancelled => self.on_cmdline_cancelled(),
            FromNeovim::CmdlineMessage(text) => self.on_cmdline_message(text),
            FromNeovim::AutoCommit(text) => self.on_auto_commit(text),
            FromNeovim::NvimExited => self.on_nvim_exited(),
        }
    }

    fn on_preedit(&mut self, info: neovim::PreeditInfo) {
        log::debug!(
            "[NVIM] Preedit: {:?}, cursor: {}..{}, mode: {}",
            info.text,
            info.cursor_begin,
            info.cursor_end,
            info.mode
        );
        if !self.ime.is_fully_enabled() {
            log::debug!("[NVIM] Ignoring Preedit (IME not fully enabled)");
            return;
        }
        self.ime
            .set_preedit(info.text, info.cursor_begin, info.cursor_end);
        self.keypress.set_vim_mode(&info.mode);
        self.keypress.recording = info.recording;
        self.update_preedit();
    }

    fn on_commit(&mut self, text: String) {
        log::debug!("[NVIM] Commit: {:?}", text);
        self.ime.clear_preedit();
        self.ime.clear_candidates();
        self.wayland.commit_string(&text);
        self.keypress.clear();
        self.keypress_timer_token = None;
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

    fn on_delete_surrounding(&mut self, before: u32, after: u32) {
        log::debug!(
            "[NVIM] DeleteSurrounding: before={}, after={}",
            before,
            after
        );
        self.wayland.delete_surrounding(before, after);
    }

    fn on_candidates(&mut self, info: neovim::CandidateInfo) {
        log::debug!(
            "[NVIM] Candidates: {:?}, selected={}",
            info.candidates,
            info.selected
        );
        if !self.ime.is_fully_enabled() {
            return;
        }
        if info.candidates.is_empty() {
            self.hide_candidates();
        } else {
            self.ime.set_candidates(info.candidates, info.selected);
            self.update_popup();
        }
    }

    fn on_visual_range(&mut self, selection: Option<neovim::VisualSelection>) {
        log::debug!("[NVIM] VisualRange: {:?}", selection);
        if !self.ime.is_fully_enabled() {
            return;
        }
        self.visual_display = selection;
        self.update_popup();
    }

    fn on_passthrough_key(&mut self) {
        // Send the current key through the virtual keyboard to the focused app
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

    fn on_cmdline_update(&mut self, text: String) {
        log::debug!("[NVIM] CmdlineUpdate: {:?}", text);
        if !self.ime.is_fully_enabled() {
            return;
        }
        self.keypress.set_display_text(text);
        self.keypress.set_vim_mode("c");
        self.update_popup();
    }

    fn on_cmdline_cancelled(&mut self) {
        log::debug!("[NVIM] CmdlineCancelled");
        self.keypress.clear();
        // Restore vim_mode to normal — command mode is only entered from normal
        // mode (`:` key), so after cancel/execution the mode is always `n`.
        // clear() doesn't reset vim_mode, so it stays "c" without this.
        self.keypress.set_vim_mode("n");
        self.keypress_timer_token = None;
        self.update_popup();
    }

    fn on_cmdline_message(&mut self, text: String) {
        log::debug!("[NVIM] CmdlineMessage: {:?}", text);
        if !self.ime.is_fully_enabled() {
            return;
        }
        self.keypress.set_display_text(text);
        self.update_popup();
    }

    fn on_auto_commit(&mut self, text: String) {
        log::debug!("[NVIM] AutoCommit: {:?}", text);
        if !self.ime.is_fully_enabled() {
            return;
        }
        self.wayland.commit_string(&text);
        self.ime.clear_preedit();
        self.ime.clear_candidates();
        self.keypress.clear();
        self.keypress_timer_token = None;
        self.visual_display = None;
        self.update_popup();
    }

    fn on_nvim_exited(&mut self) {
        log::info!("[NVIM] Neovim exited, disabling IME");
        // Clear compositor preedit (still active, compositor may show stale text)
        self.wayland.set_preedit("", 0, 0);
        self.reset_ime_state();
        self.ime.disable();
        self.nvim = None;
    }

    pub(crate) fn update_preedit(&mut self) {
        let cursor_begin = self.ime.cursor_begin as i32;
        let cursor_end = self.ime.cursor_end as i32;
        // Don't send preedit to compositor when IME is disabled or deactivated.
        if self.wayland.active && self.ime.is_enabled() {
            self.wayland
                .set_preedit(&self.ime.preedit, cursor_begin, cursor_end);
            log::debug!(
                "[PREEDIT] updated: {:?}, cursor: {}..{}",
                self.ime.preedit,
                cursor_begin,
                cursor_end
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
        self.update_popup();
    }

    /// Update the unified popup with current state
    pub(crate) fn update_popup(&mut self) {
        let t = std::time::Instant::now();
        let content = PopupContent {
            preedit: self.ime.preedit.clone(),
            cursor_begin: self.ime.cursor_begin,
            cursor_end: self.ime.cursor_end,
            vim_mode: self.keypress.vim_mode.clone(),
            keypress: if self.keypress.should_show() {
                self.keypress.display_text()
            } else {
                String::new()
            },
            candidates: self.ime.candidates.clone(),
            selected: self.ime.selected_candidate,
            visual_selection: self.visual_display.clone(),
            ime_enabled: self.ime.is_enabled(),
            recording: self.keypress.recording.clone(),
        };
        if let Some(ref mut popup) = self.popup {
            let qh = self.wayland.qh.clone();
            popup.update(&content, &qh);
        }
        log::trace!(
            "[PERF] update_popup: {:.2}ms",
            t.elapsed().as_secs_f64() * 1000.0
        );
    }

    /// Hide the unified popup
    pub(crate) fn hide_popup(&mut self) {
        if let Some(ref mut popup) = self.popup {
            popup.hide();
        }
    }

    pub(crate) fn hide_candidates(&mut self) {
        self.ime.clear_candidates();
        self.update_popup();
    }
}

#[cfg(test)]
mod replay_tests {
    use serde::Deserialize;

    use crate::neovim::{FromNeovim, VisualSelection};
    use crate::state::{ImeState, KeypressState, VimMode};

    /// Minimal state for replaying FromNeovim messages without Wayland/popup.
    struct ReplayState {
        ime: ImeState,
        keypress: KeypressState,
        visual_display: Option<VisualSelection>,
        committed: Vec<String>,
        exited: bool,
    }

    impl ReplayState {
        fn new() -> Self {
            let mut ime = ImeState::new();
            // Start as fully enabled (most replay scenarios assume enabled IME)
            ime.start_enabling();
            ime.complete_enabling(VimMode::Insert);
            Self {
                ime,
                keypress: KeypressState::new(),
                visual_display: None,
                committed: Vec::new(),
                exited: false,
            }
        }

        fn apply(&mut self, msg: FromNeovim) {
            match msg {
                FromNeovim::Ready | FromNeovim::KeyProcessed | FromNeovim::PassthroughKey => {}
                FromNeovim::DeleteSurrounding { .. } => {}
                FromNeovim::Preedit(info) => {
                    if self.ime.is_fully_enabled() {
                        self.ime
                            .set_preedit(info.text, info.cursor_begin, info.cursor_end);
                        self.keypress.set_vim_mode(&info.mode);
                        self.keypress.recording = info.recording;
                    }
                }
                FromNeovim::Commit(text) => {
                    self.committed.push(text);
                    self.ime.clear_preedit();
                    self.ime.clear_candidates();
                    self.keypress.clear();
                }
                FromNeovim::Candidates(info) => {
                    if self.ime.is_fully_enabled() {
                        if info.candidates.is_empty() {
                            self.ime.clear_candidates();
                        } else {
                            self.ime.set_candidates(info.candidates, info.selected);
                        }
                    }
                }
                FromNeovim::VisualRange(selection) => {
                    if self.ime.is_fully_enabled() {
                        self.visual_display = selection;
                    }
                }
                FromNeovim::CmdlineUpdate(text) => {
                    if self.ime.is_fully_enabled() {
                        self.keypress.set_display_text(text);
                        self.keypress.set_vim_mode("c");
                    }
                }
                FromNeovim::CmdlineCancelled => {
                    self.keypress.clear();
                    self.keypress.set_vim_mode("n");
                }
                FromNeovim::CmdlineMessage(text) => {
                    if self.ime.is_fully_enabled() {
                        self.keypress.set_display_text(text);
                    }
                }
                FromNeovim::AutoCommit(text) => {
                    if self.ime.is_fully_enabled() {
                        self.committed.push(text);
                        self.ime.clear_preedit();
                        self.ime.clear_candidates();
                        self.keypress.clear();
                        self.visual_display = None;
                    }
                }
                FromNeovim::NvimExited => {
                    self.ime.clear_preedit();
                    self.ime.clear_candidates();
                    self.keypress.clear();
                    self.keypress.recording.clear();
                    self.visual_display = None;
                    self.ime.disable();
                    self.exited = true;
                }
            }
        }
    }

    #[derive(Deserialize)]
    struct Fixture {
        #[allow(dead_code)]
        description: String,
        messages: Vec<serde_json::Value>,
        expect: Expected,
    }

    #[derive(Deserialize)]
    struct Expected {
        preedit: String,
        cursor_begin: usize,
        cursor_end: usize,
        vim_mode: String,
        candidates_count: usize,
        committed: Vec<String>,
        exited: bool,
    }

    fn run_fixture(path: &str) {
        let content = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read fixture {path}: {e}"));
        let fixture: Fixture = serde_json::from_str(&content)
            .unwrap_or_else(|e| panic!("failed to parse fixture {path}: {e}"));

        let mut state = ReplayState::new();
        for (i, value) in fixture.messages.iter().enumerate() {
            let msg: FromNeovim = serde_json::from_value(value.clone())
                .unwrap_or_else(|e| panic!("failed to parse message {i} in {path}: {e}"));
            state.apply(msg);
        }

        let expect = &fixture.expect;
        assert_eq!(
            state.ime.preedit, expect.preedit,
            "preedit mismatch in {path}"
        );
        assert_eq!(
            state.ime.cursor_begin, expect.cursor_begin,
            "cursor_begin mismatch in {path}"
        );
        assert_eq!(
            state.ime.cursor_end, expect.cursor_end,
            "cursor_end mismatch in {path}"
        );
        assert_eq!(
            state.keypress.vim_mode, expect.vim_mode,
            "vim_mode mismatch in {path}"
        );
        assert_eq!(
            state.ime.candidates.len(),
            expect.candidates_count,
            "candidates_count mismatch in {path}"
        );
        assert_eq!(
            state.committed, expect.committed,
            "committed mismatch in {path}"
        );
        assert_eq!(state.exited, expect.exited, "exited mismatch in {path}");
    }

    #[test]
    fn replay_insert_and_commit() {
        run_fixture("tests/fixtures/insert_and_commit.json");
    }

    #[test]
    fn replay_candidates_and_select() {
        run_fixture("tests/fixtures/candidates_and_select.json");
    }

    #[test]
    fn replay_cmdline_and_cancel() {
        run_fixture("tests/fixtures/cmdline_and_cancel.json");
    }

    #[test]
    fn replay_nvim_exit() {
        run_fixture("tests/fixtures/nvim_exit.json");
    }
}
