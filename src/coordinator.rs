use std::os::fd::OwnedFd;

use calloop::PostAction;
use calloop::generic::Generic;
use calloop::{Interest, Mode as Trigger};

use crate::State;
use crate::model::Effect;
use crate::neovim::{FromNeovim, Neovim, ReadStatus};
use crate::ui::{Header, MAX_GRID_ROWS, PopupContent, Theme, popupmenu_float};

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
        self.animation_timer_token = None;
        self.hide_popup();
        self.wayland.release_keyboard();
        self.keyboard.reset_modifiers();
    }

    /// Turn the IME on or off (control socket). Returns whether it is on.
    pub(crate) fn set_ime_enabled(&mut self, enable: bool) -> bool {
        let was_enabled = self.model.ime.is_enabled();
        log::info!("[IME] Set enabled: {was_enabled} -> {enable}");

        if enable && !was_enabled {
            // Respawn Neovim if it exited (e.g., after :q)
            if self.nvim.is_none() && !self.start_nvim() {
                return false;
            }
            self.model.ime.start_enabling();
            if !self.wayland.active {
                // Takes effect when a text field is focused (Activate)
                log::debug!("[IME] Enabled with no text field focused");
            } else if self.wayland.sensitive {
                // Takes effect once focus leaves the password field
                log::info!("[IME] Enabled in a password field: keys go to the application");
            } else if self.wayland.keyboard_grab.is_none() {
                log::debug!("[IME] Grabbing keyboard");
                self.wayland.grab_keyboard();
                self.keyboard.pending_keymap = true;
            }
        } else if !enable && was_enabled {
            // Disable IME - commit the buffer BEFORE releasing keyboard
            // (must match Commit handler order: commit first, then release)
            log::debug!("[IME] Releasing keyboard");
            // Whole buffer from the line-event mirror (no RPC: Neovim may be
            // blocked in getchar)
            if self.wayland.active && !self.model.view.buffer.is_empty() {
                self.wayland.commit_string(&self.model.view.buffer.text());
            }
            self.reset_ime_state();
            // Clear Neovim buffer (must clear here, not rely on Deactivate —
            // rapid re-enable can happen before Deactivate fires); with no
            // field to commit to, the text goes to the stash register
            self.clear_nvim_buffer(!self.wayland.active);
            self.model.ime.disable();
        }
        self.model.ime.is_enabled()
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
                Effect::PassthroughKey(key) => self.wayland.send_virtual_key(key),
                Effect::NvimExited => {
                    self.release_ime_resources();
                    self.nvim = None;
                }
            }
        }
    }

    /// Neovim UI size fitted to the popup, so long lines wrap at the popup
    /// width and Neovim scrolls the window itself; the whole screen's size
    /// in focus mode
    fn ui_size(&self) -> Option<(usize, usize)> {
        self.popup.as_ref().map(|popup| popup.ui_size(self.focus))
    }

    pub(crate) fn has_focus_size(&self) -> bool {
        self.popup
            .as_ref()
            .is_some_and(|popup| popup.has_focus_size())
    }

    /// Switch focus mode: Neovim's UI takes the size of the shown screen
    pub(crate) fn set_focus(&mut self, focus: bool) {
        self.focus = focus;
        if let (Some(size), Some(nvim)) = (self.ui_size(), self.nvim.as_mut()) {
            nvim.resize_ui(size);
        }
        self.update_popup();
    }

    /// Spawn Neovim and watch its stdout. Returns false if it failed.
    pub(crate) fn start_nvim(&mut self) -> bool {
        let nvim = match Neovim::spawn(&self.config, self.ui_size()) {
            Ok(nvim) => nvim,
            Err(e) => {
                log::error!("[NVIM] Failed to spawn Neovim: {e}");
                return false;
            }
        };
        // The source gets its own fd; the pipe itself stays with `Neovim`
        let fd = match nvim.read_fd().try_clone_to_owned() {
            Ok(fd) => fd,
            Err(e) => {
                log::error!("[NVIM] Failed to watch Neovim output: {e}");
                return false;
            }
        };
        let source = Generic::new(fd, Interest::READ, Trigger::Level);
        match self.loop_handle.insert_source(source, |_, _, state| {
            state.on_nvim_readable();
            // Neovim exited: this source belonged to it
            Ok(if state.nvim.is_some() {
                PostAction::Continue
            } else {
                PostAction::Remove
            })
        }) {
            Ok(_) => {
                log::info!("[NVIM] Neovim backend spawned");
                self.nvim = Some(nvim);
                true
            }
            Err(e) => {
                log::error!("[NVIM] Failed to watch Neovim output: {e}");
                false
            }
        }
    }

    /// Handle everything Neovim has sent
    fn on_nvim_readable(&mut self) {
        let Some(nvim) = self.nvim.as_mut() else {
            return;
        };
        let status = nvim.read();
        self.drain_nvim_messages();
        if status == ReadStatus::Eof && self.nvim.is_some() {
            self.handle_nvim_message(FromNeovim::NvimExited);
        }
    }

    /// Handle Neovim messages one at a time: each one's events are applied
    /// to the model before the next message is handled, which reads its mode
    fn drain_nvim_messages(&mut self) {
        loop {
            let mode = self.model.view.mode;
            let Some(nvim) = self.nvim.as_mut() else {
                return;
            };
            match nvim.next_message(mode) {
                Ok(Some(events)) => {
                    for event in events {
                        self.handle_nvim_message(event);
                    }
                }
                Ok(None) => return,
                Err(e) => {
                    log::error!("[NVIM] {e}");
                    nvim.kill();
                    self.handle_nvim_message(FromNeovim::NvimExited);
                    return;
                }
            }
        }
    }

    /// Watch stdin while requests wait for the pipe to take them
    pub(crate) fn watch_nvim_writable(&mut self) {
        let Some(ref nvim) = self.nvim else {
            return;
        };
        if !nvim.wants_write() || self.nvim_write_token.is_some() {
            return;
        }
        let fd: OwnedFd = match nvim.write_fd().try_clone_to_owned() {
            Ok(fd) => fd,
            Err(e) => {
                log::error!("[NVIM] Failed to watch Neovim input: {e}");
                return;
            }
        };
        let source = Generic::new(fd, Interest::WRITE, Trigger::Level);
        match self.loop_handle.insert_source(source, |_, _, state| {
            if let Some(ref mut nvim) = state.nvim {
                nvim.flush();
                if nvim.wants_write() {
                    return Ok(PostAction::Continue);
                }
            }
            state.nvim_write_token = None;
            Ok(PostAction::Remove)
        }) {
            Ok(token) => self.nvim_write_token = Some(token),
            Err(e) => log::error!("[NVIM] Failed to watch Neovim input: {e}"),
        }
    }

    /// Reset Neovim to an empty buffer in normal mode, or insert mode if
    /// `insert` (after keys already queued)
    pub(crate) fn reset_nvim(&mut self, insert: bool) {
        self.reset_nvim_with(insert, false);
    }

    fn reset_nvim_with(&mut self, insert: bool, stash: bool) {
        let mode = self.model.view.mode;
        let Some(nvim) = self.nvim.as_mut() else {
            return;
        };
        let events = nvim.reset(insert, stash, mode);
        for event in events {
            self.handle_nvim_message(event);
        }
    }

    /// Clear Neovim's buffer, dropping keys not processed yet (they were
    /// typed for the text being thrown away). With `stash` (the text was not
    /// committed, e.g. focus moved away), it goes to the stash register
    /// (`stash_register` in the config), if one is set.
    pub(crate) fn clear_nvim_buffer(&mut self, stash: bool) {
        if let Some(nvim) = self.nvim.as_mut() {
            for seq in nvim.discard_queued_keys() {
                self.model.sent_keys.remove(seq);
            }
        }
        self.reset_nvim_with(false, stash);
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
        // After toggle-off, Neovim sends a burst of push notifications (clearing
        // the buffer triggers mode changes and redraws) — without this guard,
        // each notification would rebuild PopupContent and potentially
        // recreate/destroy surfaces.
        // Without the grab (no text field, or a password field) keys don't
        // pass through jacin, so there is nothing to show
        if !self.model.ime.is_enabled() || self.wayland.keyboard_grab.is_none() {
            self.hide_popup();
            return;
        }
        let t = std::time::Instant::now();
        let view = &self.model.view;
        let header = match (&view.cmdline, view.visible_showcmd().is_empty()) {
            (Some(cmdline), _) => Some(Header::Cmdline {
                text: cmdline.text.clone(),
                cursor_byte: cmdline.cursor_byte,
            }),
            (None, false) => Some(Header::ShowCmd(view.visible_showcmd().to_owned())),
            (None, true) => None,
        };
        let theme = Theme::from_screen(&view.screen, self.config.appearance.opacity);
        let mut candidates = view.visible_candidates().to_vec();
        let mut selected = view.selected_candidate.filter(|_| !candidates.is_empty());
        let screen = &view.screen;
        let mut window_view = if self.focus {
            screen.screen_view()
        } else {
            screen.window_view(MAX_GRID_ROWS)
        };
        match &window_view {
            Some(v) => log::debug!(
                "[POPUP] window grid: {} rows, cursor={:?} (visible={}, float={})",
                v.rows.len(),
                v.cursor,
                v.cursor_visible,
                v.cursor_on_float
            ),
            None => log::debug!("[POPUP] no window grid yet"),
        }
        // The whole screen is large: the menu goes next to the word, as
        // Neovim would draw it, instead of the panel far from the cursor
        if self.focus
            && let (Some(v), Some(anchor), Some(global)) =
                (window_view.as_mut(), view.candidates_anchor, screen.grid(1))
            && let Some(at) = screen.screen_position(anchor.grid, anchor.row, anchor.col)
            && let Some(float) = popupmenu_float(
                &candidates,
                selected,
                at,
                (global.height(), global.width()),
                &theme,
            )
        {
            v.floats.push(float);
            v.floats.sort_by_key(|f| f.zindex);
            candidates.clear();
            selected = None;
        }
        let content = PopupContent {
            mode: view.mode,
            header,
            message: view.transient_message.clone(),
            selected,
            candidates,
            ime_enabled: self.model.ime.is_enabled(),
            recording: self.model.view.recording.clone(),
            rec_blink_on: self.animations.rec_blink.on,
            theme,
            window_view,
            focus: self.focus,
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
