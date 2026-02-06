use std::os::fd::{AsFd, AsRawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use calloop::{
    EventLoop, LoopSignal,
    ping::make_ping,
    signals::{Signal, Signals},
    timer::{TimeoutAction, Timer},
};
use calloop_wayland_source::WaylandSource;
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_buffer, wl_compositor, wl_keyboard, wl_registry, wl_shm, wl_shm_pool, wl_surface,
    },
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2, zwp_input_method_manager_v2, zwp_input_method_v2,
    zwp_input_popup_surface_v2,
};
use xkbcommon::xkb;

mod config;
mod neovim;
mod state;
mod ui;

use neovim::{FromNeovim, NeovimHandle, OldFromNeovim, PendingState, pending_state};
use state::{ImeState, KeyboardState, KeypressState, WaylandState};
use ui::{PopupContent, TextRenderer, UnifiedPopup};

// Helper to convert new FromNeovim to old format during transition
fn convert_nvim_msg(msg: FromNeovim) -> OldFromNeovim {
    msg.into()
}

fn main() -> anyhow::Result<()> {
    // Load configuration
    let config = config::Config::load();

    // Connect to Wayland display
    let conn = Connection::connect_to_env()?;
    eprintln!("Connected to Wayland display");

    // Initialize registry and get globals
    let (globals, event_queue) = registry_queue_init::<State>(&conn)?;
    let qh = event_queue.handle();

    // Bind input method manager
    let input_method_manager: zwp_input_method_manager_v2::ZwpInputMethodManagerV2 = globals
        .bind(&qh, 1..=1, ())
        .expect("zwp_input_method_manager_v2 not available - is this a wlroots compositor?");
    eprintln!("Bound zwp_input_method_manager_v2");

    // Get the seat (assuming single seat)
    let seat: wayland_client::protocol::wl_seat::WlSeat =
        globals.bind(&qh, 1..=9, ()).expect("wl_seat not available");

    // Bind compositor and shm for candidate window
    let compositor: wl_compositor::WlCompositor = globals
        .bind(&qh, 4..=6, ())
        .expect("wl_compositor not available");

    let shm: wl_shm::WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm not available");

    // Create input method for this seat
    let input_method = input_method_manager.get_input_method(&seat, &qh, ());
    eprintln!("Created zwp_input_method_v2");

    // Spawn Neovim backend
    let nvim = match neovim::spawn_neovim(config.clone()) {
        Ok(handle) => {
            eprintln!("Neovim backend spawned");
            Some(handle)
        }
        Err(e) => {
            eprintln!("Failed to spawn Neovim: {} (continuing without backend)", e);
            None
        }
    };

    // Try to create text renderer for unified popup window
    let text_renderer = TextRenderer::new(16.0);
    if text_renderer.is_none() {
        eprintln!("Warning: Font not available, popup window disabled");
    }

    // Create unified popup window using input method popup surface
    // The popup surface is automatically positioned near the cursor by the compositor
    let popup = if let Some(renderer) = text_renderer {
        match UnifiedPopup::new(&compositor, &input_method, &shm, &qh, renderer) {
            Some(win) => {
                eprintln!("Unified popup window created (using input popup surface)");
                Some(win)
            }
            None => {
                eprintln!("Failed to create unified popup window");
                None
            }
        }
    } else {
        None
    };

    // Create application state
    let mut state = State {
        loop_signal: None,
        wayland: WaylandState::new(qh.clone(), input_method),
        keyboard: KeyboardState::new(),
        ime: ImeState::new(),
        keypress: KeypressState::new(),
        pending_exit: false,
        toggle_flag: Arc::new(AtomicBool::new(false)),
        reactivation_count: 0,
        nvim,
        popup,
        config,
    };

    // Set up calloop event loop
    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    state.loop_signal = Some(event_loop.get_signal());

    // Insert Wayland event source
    WaylandSource::new(conn, event_queue).insert(event_loop.handle())?;

    // Set up signal handling for clean exit
    let loop_signal = state.loop_signal.clone();
    let exit_signals = Signals::new(&[Signal::SIGINT, Signal::SIGTERM])?;
    event_loop
        .handle()
        .insert_source(exit_signals, move |_, _, _| {
            eprintln!("\nReceived signal, exiting...");
            if let Some(ref signal) = loop_signal {
                signal.stop();
            }
        })?;

    // Set up SIGUSR1 for IME toggle (triggered by: pkill -SIGUSR1 custom-ime)
    // Use a ping to wake up the event loop when signal arrives
    let (ping, ping_source) = make_ping()?;
    let toggle_flag_clone = state.toggle_flag.clone();

    // Register signal handler that sets flag AND pings the event loop
    let ping_clone = ping.clone();
    unsafe {
        signal_hook::low_level::register(signal_hook::consts::SIGUSR1, move || {
            toggle_flag_clone.store(true, Ordering::SeqCst);
            ping_clone.ping();
        })?;
    }

    // Add ping source to event loop (just to wake it up, we handle toggle in the callback)
    event_loop
        .handle()
        .insert_source(ping_source, |_, _, _| {})?;

    // Add timer for keypress display timeout (fires every 100ms to check)
    let timer = Timer::from_duration(std::time::Duration::from_millis(100));
    event_loop
        .handle()
        .insert_source(timer, |_, _, state| {
            // Check for keypress display timeout
            if state.keypress.should_show() && state.keypress.is_timed_out() {
                state.hide_keypress();
            }
            // Re-arm the timer to fire again
            TimeoutAction::ToDuration(std::time::Duration::from_millis(100))
        })
        .expect("Failed to insert timer source");

    // Small delay to let any pending key events (like Enter from "cargo run") clear
    std::thread::sleep(std::time::Duration::from_millis(500));

    eprintln!("Entering event loop... (Ctrl+C to exit)");
    eprintln!("Focus a text input field to activate the IME");

    // Run the event loop
    event_loop.run(None, &mut state, |state| {
        // Check for IME toggle signal (SIGUSR1)
        if state.toggle_flag.swap(false, Ordering::SeqCst) {
            state.handle_ime_toggle();
        }

        // Check for messages from Neovim
        // Collect messages first to avoid borrow conflict
        let messages: Vec<_> = state
            .nvim
            .as_ref()
            .map(|nvim| std::iter::from_fn(|| nvim.try_recv()).collect())
            .unwrap_or_default();

        for msg in messages {
            state.handle_nvim_message(convert_nvim_msg(msg));
        }

        if state.pending_exit
            && let Some(ref signal) = state.loop_signal
        {
            signal.stop();
        }
    })?;

    // Cleanup
    state.wayland.release_keyboard();
    if let Some(ref nvim) = state.nvim {
        nvim.shutdown();
    }
    if let Some(window) = state.popup.take() {
        window.destroy();
    }

    eprintln!("Goodbye!");

    // Force clean exit to avoid any stuck keyboard state
    std::process::exit(0);
}

pub struct State {
    loop_signal: Option<LoopSignal>,
    // Component state structs
    wayland: WaylandState,
    keyboard: KeyboardState,
    ime: ImeState,
    keypress: KeypressState,
    // Exit and toggle flags
    pending_exit: bool,
    toggle_flag: Arc<AtomicBool>,
    // Counter for consecutive Deactivate/Activate re-grabs without user key input.
    // Prevents infinite loop when compositor keeps cycling.
    reactivation_count: u32,
    // Neovim backend
    nvim: Option<NeovimHandle>,
    // Unified popup window (preedit, keypress, candidates)
    popup: Option<UnifiedPopup>,
    // Configuration
    config: config::Config,
}

impl State {
    fn handle_ime_toggle(&mut self) {
        let was_enabled = self.ime.is_enabled();
        eprintln!("[IME] Toggle: was_enabled = {}", was_enabled);
        self.reactivation_count = 0;

        if !was_enabled {
            // Enable IME - grab keyboard, skkeleton toggle will be sent after keymap loads
            if self.wayland.active && self.wayland.keyboard_grab.is_none() {
                eprintln!("[IME] Grabbing keyboard");
                self.wayland.grab_keyboard();
                self.keyboard.pending_keymap = true;
                self.ime.start_enabling(true); // Will enable skkeleton after keymap
            }
        } else {
            // Disable IME - commit preedit text, release keyboard, disable skkeleton
            eprintln!("[IME] Releasing keyboard");
            // Commit any pending preedit text BEFORE releasing keyboard
            // (must match Commit handler order: commit first, then release)
            if !self.ime.preedit.is_empty() {
                self.wayland.commit_string(&self.ime.preedit);
            }
            self.wayland.release_keyboard();
            // Send toggle to Neovim to disable skkeleton, then clear buffer.
            // Must clear here rather than relying on Deactivate handler,
            // because rapid re-enable can happen before Deactivate fires.
            if let Some(ref nvim) = self.nvim {
                nvim.send_key(&self.config.keybinds.toggle);
                nvim.send_key("<Esc>ggdG");
            }
            // Clear preedit and keypress display
            self.ime.clear_preedit();
            self.keypress.clear();
            self.hide_popup();
            self.ime.disable();
        }
    }

    fn handle_key(&mut self, key: u32, key_state: wl_keyboard::KeyState) {
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
        let vim_key = self.keysym_to_vim(keysym, &utf8);
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

    /// Update the unified popup with current state
    fn update_popup(&mut self) {
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
        };
        if let Some(ref mut popup) = self.popup {
            let qh = self.wayland.qh.clone();
            popup.update(&content, &qh);
        }
    }

    /// Hide the unified popup
    fn hide_popup(&mut self) {
        if let Some(ref mut popup) = self.popup {
            popup.hide();
        }
    }

    fn show_candidates(&mut self) {
        self.update_popup();
    }

    fn hide_candidates(&mut self) {
        self.ime.clear_candidates();
        self.update_popup();
    }

    fn show_keypress(&mut self) {
        self.update_popup();
    }

    fn hide_keypress(&mut self) {
        self.keypress.clear();
        self.update_popup();
    }

    fn show_preedit_window(&mut self) {
        self.update_popup();
    }

    fn update_keypress_from_pending(&mut self) {
        // Sync keypress state with neovim pending state
        let state = pending_state().load();
        if state.is_pending() {
            self.keypress.set_pending(state);
        } else {
            self.hide_keypress();
        }
    }

    fn update_preedit(&mut self) {
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
            eprintln!(
                "[PREEDIT] updated: {:?}, cursor: {}..{}",
                self.ime.preedit, cursor_begin, cursor_end
            );
        } else {
            eprintln!(
                "[PREEDIT] skipped (active={}, enabled={}): {:?}",
                self.wayland.active,
                self.ime.is_enabled(),
                self.ime.preedit
            );
        }
        // Show preedit window with cursor visualization
        self.show_preedit_window();
    }

    fn handle_nvim_message(&mut self, msg: OldFromNeovim) {
        match msg {
            OldFromNeovim::Ready => {
                eprintln!("[NVIM] Backend ready!");
            }
            OldFromNeovim::Preedit(text, cursor_begin, cursor_end, mode) => {
                eprintln!(
                    "[NVIM] Preedit: {:?}, cursor: {}..{}, mode: {}",
                    text, cursor_begin, cursor_end, mode
                );
                self.ime.set_preedit(text, cursor_begin, cursor_end);
                self.keypress.set_vim_mode(&mode);
                self.update_preedit();
            }
            OldFromNeovim::Commit(text) => {
                eprintln!("[NVIM] Commit: {:?}", text);
                self.ime.clear_preedit();
                self.ime.clear_candidates();
                self.wayland.commit_string(&text);
                // Hide popup on commit
                self.hide_popup();
                // Release keyboard grab and go back to passthrough mode
                self.wayland.release_keyboard();
                self.keypress.clear();
                self.ime.disable();
                // Consume any pending toggle (e.g., Alt in commit key <A-;> also
                // triggers SIGUSR1 toggle — don't let it re-enable after commit)
                self.toggle_flag.store(false, Ordering::SeqCst);
                // Reset Neovim buffer for next input session
                if let Some(ref nvim) = self.nvim {
                    nvim.send_key("<Esc>ggdG");
                }
            }
            OldFromNeovim::DeleteSurrounding(before, after) => {
                eprintln!(
                    "[NVIM] DeleteSurrounding: before={}, after={}",
                    before, after
                );
                self.wayland.delete_surrounding(before, after);
            }
            OldFromNeovim::Candidates(candidates, selected) => {
                eprintln!("[NVIM] Candidates: {:?}, selected={}", candidates, selected);
                if candidates.is_empty() {
                    self.hide_candidates();
                } else {
                    self.ime.set_candidates(candidates, selected);
                    self.show_candidates();
                }
            }
        }
    }

    fn send_to_nvim(&self, key: &str) {
        if let Some(ref nvim) = self.nvim {
            nvim.send_key(key);
        }
    }

    fn wait_for_nvim_response(&mut self) {
        if let Some(ref nvim) = self.nvim {
            // Block waiting for response with 200ms timeout
            if let Some(msg) = nvim.recv_timeout(std::time::Duration::from_millis(200)) {
                self.handle_nvim_message(convert_nvim_msg(msg));
            }
        }
    }

    fn keysym_to_vim(&self, keysym: xkb::Keysym, utf8: &str) -> Option<String> {
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
        if self.keyboard.alt_pressed {
            if let Some(key) = base_key {
                return Some(format!("<A-{}>", key));
            }
            if !utf8.is_empty() && !utf8.chars().all(|c| c.is_control()) {
                return Some(format!("<A-{}>", utf8));
            }
            return None;
        }

        // Handle Ctrl combinations
        if self.keyboard.ctrl_pressed {
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

    fn update_modifiers(
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
}

// ============================================================================
// Dispatch implementations for Wayland protocols
// ============================================================================

// Dispatch for registry (required by registry_queue_init)
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _state: &mut Self,
        _registry: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // Globals are handled by GlobalListContents
    }
}

// Dispatch for seat
impl Dispatch<wayland_client::protocol::wl_seat::WlSeat, ()> for State {
    fn event(
        _state: &mut Self,
        _seat: &wayland_client::protocol::wl_seat::WlSeat,
        _event: wayland_client::protocol::wl_seat::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // Seat events (capabilities, name) - we don't need to handle these
    }
}

// Dispatch for compositor
impl Dispatch<wl_compositor::WlCompositor, ()> for State {
    fn event(
        _state: &mut Self,
        _compositor: &wl_compositor::WlCompositor,
        _event: wl_compositor::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // Compositor has no events
    }
}

// Dispatch for shm
impl Dispatch<wl_shm::WlShm, ()> for State {
    fn event(
        _state: &mut Self,
        _shm: &wl_shm::WlShm,
        event: wl_shm::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_shm::Event::Format { format } = event {
            eprintln!("[SHM] Format available: {:?}", format);
        }
    }
}

// Dispatch for shm pool
impl Dispatch<wl_shm_pool::WlShmPool, ()> for State {
    fn event(
        _state: &mut Self,
        _pool: &wl_shm_pool::WlShmPool,
        _event: wl_shm_pool::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // Pool has no events
    }
}

// Dispatch for surface
impl Dispatch<wl_surface::WlSurface, ()> for State {
    fn event(
        _state: &mut Self,
        _surface: &wl_surface::WlSurface,
        event: wl_surface::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_surface::Event::Enter { .. } => {
                eprintln!("[SURFACE] Entered output");
            }
            wl_surface::Event::Leave { .. } => {
                eprintln!("[SURFACE] Left output");
            }
            _ => {}
        }
    }
}

// Dispatch for buffer (with buffer index as user data)
// Unified popup uses indices 0 and 1 for double buffering
impl Dispatch<wl_buffer::WlBuffer, usize> for State {
    fn event(
        state: &mut Self,
        _buffer: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        data: &usize,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            eprintln!("[BUFFER] Released: {}", data);
            if *data < 2
                && let Some(ref mut popup) = state.popup
            {
                popup.buffer_released(*data);
            }
        }
    }
}

// Dispatch for input popup surface (candidate window)
impl Dispatch<zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2, ()> for State {
    fn event(
        _state: &mut Self,
        _popup_surface: &zwp_input_popup_surface_v2::ZwpInputPopupSurfaceV2,
        event: zwp_input_popup_surface_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zwp_input_popup_surface_v2::Event::TextInputRectangle {
            x,
            y,
            width,
            height,
        } = event
        {
            // The compositor tells us where the text cursor is
            // This is informational - positioning is handled by the compositor
            eprintln!(
                "[POPUP] Text input rectangle: x={}, y={}, {}x{}",
                x, y, width, height
            );
        }
    }
}

// Dispatch for input method manager
impl Dispatch<zwp_input_method_manager_v2::ZwpInputMethodManagerV2, ()> for State {
    fn event(
        _state: &mut Self,
        _manager: &zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
        _event: zwp_input_method_manager_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // Manager has no events
    }
}

// Dispatch for input method - this is where the action happens!
impl Dispatch<zwp_input_method_v2::ZwpInputMethodV2, ()> for State {
    fn event(
        state: &mut Self,
        _input_method: &zwp_input_method_v2::ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => {
                state.wayland.active = true;
                eprintln!("IME activated!");

                // Re-grab keyboard if IME was enabled before deactivation.
                // Limit consecutive re-grabs to prevent infinite Deactivate/Activate
                // loops (the grab itself can trigger compositor re-evaluation).
                if state.ime.is_enabled() && state.wayland.keyboard_grab.is_none() {
                    if state.reactivation_count < 2 {
                        state.reactivation_count += 1;
                        eprintln!("[IME] Re-grabbing keyboard after activation (count={})", state.reactivation_count);
                        state.wayland.grab_keyboard();
                        state.keyboard.pending_keymap = true;
                        // false = don't toggle skkeleton (already enabled), just restore insert mode
                        state.ime.start_enabling(false);
                    } else {
                        eprintln!("[IME] Skipping re-grab (too many consecutive reactivations), disabling");
                        state.ime.disable();
                        state.reactivation_count = 0;
                    }
                }
            }
            zwp_input_method_v2::Event::Deactivate => {
                eprintln!("IME deactivated");
                state.wayland.active = false;
                // Only do cleanup when IME is enabled — avoids flooding Neovim
                // during rapid compositor activate/deactivate cycles (window switching)
                if state.ime.is_enabled() {
                    // Release keyboard grab to stop receiving key events while deactivated
                    state.wayland.release_keyboard();
                    // Clear local state (don't send Wayland protocol requests while deactivated,
                    // the compositor automatically clears preedit on deactivate)
                    state.ime.clear_preedit();
                    state.ime.clear_candidates();
                    state.keypress.clear();
                    state.hide_popup();
                    // Clear Neovim buffer to reset state for next activation
                    if let Some(ref nvim) = state.nvim {
                        nvim.send_key("<Esc>ggdG");
                    }
                }
            }
            zwp_input_method_v2::Event::SurroundingText { .. } => {
                // Noisy, don't print
            }
            zwp_input_method_v2::Event::TextChangeCause { .. } => {
                // Noisy, don't print
            }
            zwp_input_method_v2::Event::ContentType { .. } => {
                // Content type info available if needed
            }
            zwp_input_method_v2::Event::Done => {
                // Serial must equal the number of Done events received
                // (required by the commit request protocol)
                state.wayland.serial += 1;
            }
            zwp_input_method_v2::Event::Unavailable => {
                eprintln!("IME unavailable - another IME may be running");
                if let Some(signal) = &state.loop_signal {
                    signal.stop();
                }
            }
            _ => {}
        }
    }
}

// Dispatch for keyboard grab
impl Dispatch<zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2, ()> for State {
    fn event(
        state: &mut Self,
        _grab: &zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_keyboard_grab_v2::Event::Keymap { format, fd, size } => {
                eprintln!("Keymap received: format={:?}, size={}", format, size);

                if let WEnum::Value(wl_keyboard::KeymapFormat::XkbV1) = format {
                    // Memory-map the keymap (fd is borrowed, we don't own it)
                    let keymap_data =
                        unsafe { memmap_keymap(fd.as_fd().as_raw_fd(), size as usize) };

                    if let Some(data) = keymap_data {
                        // Parse the keymap using KeyboardState
                        if state.keyboard.load_keymap(&data) {
                            eprintln!("Keymap loaded successfully");

                            // Complete enabling if transitioning
                            let should_toggle = state.ime.complete_enabling();
                            if should_toggle {
                                // Set ready_time for debouncing
                                state.keyboard.mark_ready();
                                if let Some(ref nvim) = state.nvim {
                                    eprintln!("[IME] Sending skkeleton toggle");
                                    nvim.send_key(&state.config.keybinds.toggle);
                                }
                            } else if state.ime.is_fully_enabled() {
                                // Re-activation after deactivate/activate cycle:
                                // Neovim is in normal mode from <Esc>ggdG, restore insert mode
                                state.keyboard.mark_ready();
                                if let Some(ref nvim) = state.nvim {
                                    eprintln!("[IME] Restoring insert mode after re-activation");
                                    nvim.send_key("<Esc>i");
                                }
                            }
                        } else {
                            eprintln!("Failed to parse keymap");
                        }
                    }
                }
            }
            zwp_input_method_keyboard_grab_v2::Event::Key {
                serial: _,
                time: _,
                key,
                state: key_state,
            } => {
                eprintln!("[GRAB] Key event: key={}, state={:?}", key, key_state);
                // User interaction: reset reactivation counter
                state.reactivation_count = 0;
                if let WEnum::Value(ks) = key_state {
                    state.handle_key(key, ks);
                }
            }
            zwp_input_method_keyboard_grab_v2::Event::Modifiers {
                serial: _,
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                state.update_modifiers(mods_depressed, mods_latched, mods_locked, group);
            }
            zwp_input_method_keyboard_grab_v2::Event::RepeatInfo { rate, delay } => {
                eprintln!("Repeat info: rate={}/s, delay={}ms", rate, delay);
            }
            _ => {}
        }
    }
}

/// Memory-map a keymap file descriptor
unsafe fn memmap_keymap(fd: std::os::fd::RawFd, size: usize) -> Option<String> {
    unsafe {
        let ptr = libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            fd,
            0,
        );

        if ptr == libc::MAP_FAILED {
            return None;
        }

        let slice = std::slice::from_raw_parts(ptr as *const u8, size);
        // Find null terminator or use full size
        let len = slice.iter().position(|&b| b == 0).unwrap_or(size);
        let result = String::from_utf8_lossy(&slice[..len]).into_owned();

        libc::munmap(ptr, size);

        Some(result)
    }
}
