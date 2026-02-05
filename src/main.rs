use std::os::fd::{AsFd, AsRawFd};

use calloop::{
    EventLoop, LoopSignal,
    signals::{Signal, Signals},
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

mod candidate_window;
mod neovim;
mod text_render;

use candidate_window::CandidateWindow;
use neovim::{FromNeovim, NeovimHandle};
use text_render::TextRenderer;

fn main() -> anyhow::Result<()> {
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

    // Create xkb context
    let xkb_context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);

    // Create input method for this seat
    let input_method = input_method_manager.get_input_method(&seat, &qh, ());
    eprintln!("Created zwp_input_method_v2");

    // Spawn Neovim backend
    let nvim = match neovim::spawn_neovim() {
        Ok(handle) => {
            eprintln!("Neovim backend spawned");
            Some(handle)
        }
        Err(e) => {
            eprintln!("Failed to spawn Neovim: {} (continuing without backend)", e);
            None
        }
    };

    // Try to create text renderer for candidate window
    let text_renderer = TextRenderer::new(16.0);
    if text_renderer.is_none() {
        eprintln!("Warning: Font not available, candidate window disabled");
    }

    // Create candidate window using input method popup surface
    // The popup surface is automatically positioned near the cursor by the compositor
    let candidate_window = if let Some(renderer) = text_renderer {
        match CandidateWindow::new(&compositor, &input_method, &shm, &qh, renderer) {
            Some(win) => {
                eprintln!("Candidate window created (using input popup surface)");
                Some(win)
            }
            None => {
                eprintln!("Failed to create candidate window");
                None
            }
        }
    } else {
        None
    };

    // Create application state
    let mut state = State {
        loop_signal: None,
        qh: qh.clone(),
        input_method,
        keyboard_grab: None,
        xkb_context,
        xkb_state: None,
        active: false,
        serial: 0,
        ctrl_pressed: false,
        pending_exit: false,
        preedit: String::new(),
        nvim,
        candidate_window,
        candidates: Vec::new(),
        selected_candidate: 0,
    };

    // Set up calloop event loop
    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    state.loop_signal = Some(event_loop.get_signal());

    // Insert Wayland event source
    WaylandSource::new(conn, event_queue).insert(event_loop.handle())?;

    // Set up signal handling for clean exit
    let loop_signal = state.loop_signal.clone();
    let signals = Signals::new(&[Signal::SIGINT, Signal::SIGTERM])?;
    event_loop.handle().insert_source(signals, move |_, _, _| {
        eprintln!("\nReceived signal, exiting...");
        if let Some(ref signal) = loop_signal {
            signal.stop();
        }
    })?;

    // Small delay to let any pending key events (like Enter from "cargo run") clear
    std::thread::sleep(std::time::Duration::from_millis(500));

    eprintln!("Entering event loop... (Ctrl+C to exit)");
    eprintln!("Focus a text input field to activate the IME");

    // Run the event loop
    event_loop.run(None, &mut state, |state| {
        // Check for messages from Neovim
        // Collect messages first to avoid borrow conflict
        let messages: Vec<_> = state
            .nvim
            .as_ref()
            .map(|nvim| std::iter::from_fn(|| nvim.try_recv()).collect())
            .unwrap_or_default();

        for msg in messages {
            state.handle_nvim_message(msg);
        }

        if state.pending_exit
            && let Some(ref signal) = state.loop_signal
        {
            signal.stop();
        }
    })?;

    // Cleanup
    if let Some(grab) = state.keyboard_grab.take() {
        grab.release();
    }
    if let Some(ref nvim) = state.nvim {
        nvim.shutdown();
    }
    if let Some(window) = state.candidate_window.take() {
        window.destroy();
    }

    eprintln!("Goodbye!");

    // Force clean exit to avoid any stuck keyboard state
    std::process::exit(0);
}

pub struct State {
    loop_signal: Option<LoopSignal>,
    qh: QueueHandle<State>,
    input_method: zwp_input_method_v2::ZwpInputMethodV2,
    keyboard_grab: Option<zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2>,
    xkb_context: xkb::Context,
    xkb_state: Option<xkb::State>,
    active: bool,
    serial: u32,
    ctrl_pressed: bool,
    pending_exit: bool,
    // Preedit state
    preedit: String,
    // Neovim backend
    nvim: Option<NeovimHandle>,
    // Candidate window
    candidate_window: Option<CandidateWindow>,
    candidates: Vec<String>,
    selected_candidate: usize,
}

impl State {
    fn handle_key(&mut self, key: u32, key_state: wl_keyboard::KeyState) {
        let state_str = match key_state {
            wl_keyboard::KeyState::Pressed => "pressed",
            wl_keyboard::KeyState::Released => "released",
            _ => "unknown",
        };
        eprintln!(
            "[KEY] code={}, state={}, ctrl={}",
            key, state_str, self.ctrl_pressed
        );

        // Only handle key presses, not releases
        if key_state != wl_keyboard::KeyState::Pressed {
            return;
        }

        let Some(xkb_state) = &self.xkb_state else {
            eprintln!("No xkb state, cannot process key");
            return;
        };

        // Convert evdev keycode to xkb keycode (evdev + 8)
        let keycode = xkb::Keycode::new(key + 8);
        let keysym = xkb_state.key_get_one_sym(keycode);
        let utf8 = xkb_state.key_get_utf8(keycode);
        eprintln!("[KEY] keysym={:?}, utf8={:?}", keysym, utf8);

        // Handle Ctrl+C to exit
        use xkbcommon::xkb::Keysym;
        if self.ctrl_pressed && keysym == Keysym::c {
            eprintln!("\nCtrl+C pressed, releasing keyboard and exiting...");
            if let Some(grab) = self.keyboard_grab.take() {
                grab.release();
            }
            self.pending_exit = true;
            return;
        }

        // Convert key to Vim notation and send to Neovim
        let vim_key = self.keysym_to_vim(keysym, &utf8);
        eprintln!("[KEY] vim_key={:?}", vim_key);

        if let Some(ref vim_key) = vim_key {
            self.send_to_nvim(vim_key);
            // Wait for Neovim response with timeout
            self.wait_for_nvim_response();
        } else if !utf8.is_empty() && !utf8.chars().all(|c| c.is_control()) {
            // Fallback: if no Neovim or no vim key, use local preedit
            if self.nvim.is_none() {
                self.preedit.push_str(&utf8);
                eprintln!("[PREEDIT] buffer={:?}", self.preedit);
                self.update_preedit();
            }
        } else {
            eprintln!("[SKIP] no printable char, ctrl={}", self.ctrl_pressed);
        }
    }

    fn show_candidates(&mut self) {
        if let Some(ref mut window) = self.candidate_window {
            let qh = self.qh.clone();
            window.show(&self.candidates, self.selected_candidate, &qh);
        }
    }

    fn hide_candidates(&mut self) {
        self.candidates.clear();
        self.selected_candidate = 0;
        if let Some(ref mut window) = self.candidate_window {
            window.hide();
        }
    }

    fn update_preedit(&mut self) {
        let cursor_pos = self.preedit.len() as i32;
        self.input_method
            .set_preedit_string(self.preedit.clone(), cursor_pos, cursor_pos);
        self.serial += 1;
        self.input_method.commit(self.serial);
        eprintln!("[PREEDIT] updated: {:?}", self.preedit);
    }

    fn handle_nvim_message(&mut self, msg: FromNeovim) {
        match msg {
            FromNeovim::Ready => {
                eprintln!("[NVIM] Backend ready!");
            }
            FromNeovim::Preedit(text) => {
                eprintln!("[NVIM] Preedit: {:?}", text);
                self.preedit = text;
                self.update_preedit();
            }
            FromNeovim::Commit(text) => {
                eprintln!("[NVIM] Commit: {:?}", text);
                self.preedit.clear();
                self.input_method.commit_string(text);
                self.input_method.set_preedit_string(String::new(), 0, 0);
                self.serial += 1;
                self.input_method.commit(self.serial);
                // Hide candidates on commit
                self.hide_candidates();
            }
            FromNeovim::DeleteSurrounding(before, after) => {
                eprintln!(
                    "[NVIM] DeleteSurrounding: before={}, after={}",
                    before, after
                );
                self.input_method.delete_surrounding_text(before, after);
                self.serial += 1;
                self.input_method.commit(self.serial);
            }
            FromNeovim::Candidates(candidates, selected) => {
                eprintln!("[NVIM] Candidates: {:?}, selected={}", candidates, selected);
                if candidates.is_empty() {
                    self.hide_candidates();
                } else {
                    self.candidates = candidates;
                    self.selected_candidate = selected;
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
                self.handle_nvim_message(msg);
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

        // Handle Ctrl combinations
        if self.ctrl_pressed {
            if let Some(key) = base_key {
                return Some(format!("<C-{}>", key));
            }
            return None;
        }

        // Non-Ctrl: wrap special keys in <>, return letters/printable as-is
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
        // Ctrl modifier is typically bit 2 (0x4)
        const CTRL_MASK: u32 = 0x4;
        let old_ctrl = self.ctrl_pressed;
        self.ctrl_pressed = (mods_depressed & CTRL_MASK) != 0;

        if old_ctrl != self.ctrl_pressed {
            eprintln!("[MOD] ctrl changed: {} -> {}", old_ctrl, self.ctrl_pressed);
        }

        if let Some(xkb_state) = &mut self.xkb_state {
            xkb_state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
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
            if let Some(ref mut window) = state.candidate_window {
                window.buffer_released(*data);
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
        input_method: &zwp_input_method_v2::ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _data: &(),
        _conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => {
                state.active = true;
                eprintln!("IME activated!");

                // Grab the keyboard to intercept key events
                if state.keyboard_grab.is_none() {
                    let grab = input_method.grab_keyboard(qh, ());
                    state.keyboard_grab = Some(grab);
                    eprintln!("Keyboard grabbed");
                }
            }
            zwp_input_method_v2::Event::Deactivate => {
                eprintln!("IME deactivated");
                state.active = false;
                // Hide candidates when deactivated
                state.hide_candidates();
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
                // Don't print this every time, it's noisy
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
                        // Parse the keymap
                        if let Some(keymap) = xkb::Keymap::new_from_string(
                            &state.xkb_context,
                            data,
                            xkb::KEYMAP_FORMAT_TEXT_V1,
                            xkb::KEYMAP_COMPILE_NO_FLAGS,
                        ) {
                            state.xkb_state = Some(xkb::State::new(&keymap));
                            eprintln!("Keymap loaded successfully");
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
                state: WEnum::Value(ks),
            } => {
                state.handle_key(key, ks);
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
