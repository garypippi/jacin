use std::os::fd::{AsFd, AsRawFd};

use calloop::{
    EventLoop, LoopSignal,
    signals::{Signal, Signals},
};
use calloop_wayland_source::WaylandSource;
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_keyboard, wl_registry},
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2, zwp_input_method_manager_v2, zwp_input_method_v2,
};
use xkbcommon::xkb;

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

    // Create xkb context
    let xkb_context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);

    // Create input method for this seat
    let input_method = input_method_manager.get_input_method(&seat, &qh, ());
    eprintln!("Created zwp_input_method_v2");

    // Create application state
    let mut state = State {
        loop_signal: None,
        input_method,
        keyboard_grab: None,
        xkb_context,
        xkb_state: None,
        active: false,
        serial: 0,
        ctrl_pressed: false,
        pending_exit: false,
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
        if state.pending_exit && let Some(ref signal) = state.loop_signal {
            signal.stop();
        }
    })?;

    // Cleanup already done in signal handlers, but ensure it's done
    if let Some(grab) = state.keyboard_grab.take() {
        grab.release();
    }

    eprintln!("Goodbye!");

    // Force clean exit to avoid any stuck keyboard state
    std::process::exit(0);
}

struct State {
    loop_signal: Option<LoopSignal>,
    input_method: zwp_input_method_v2::ZwpInputMethodV2,
    keyboard_grab: Option<zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2>,
    xkb_context: xkb::Context,
    xkb_state: Option<xkb::State>,
    active: bool,
    serial: u32,
    ctrl_pressed: bool,
    pending_exit: bool,
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
            // Release keyboard grab first to restore normal keyboard state
            if let Some(grab) = self.keyboard_grab.take() {
                grab.release();
            }
            // Mark for exit - will happen on next event loop iteration after flush
            self.pending_exit = true;
            return;
        }

        match keysym {
            Keysym::BackSpace => {
                // Delete one character before cursor
                self.input_method.delete_surrounding_text(1, 0);
                self.serial += 1;
                self.input_method.commit(self.serial);
                return;
            }
            Keysym::Return | Keysym::KP_Enter => {
                self.input_method.commit_string("\n".to_string());
                self.serial += 1;
                self.input_method.commit(self.serial);
                return;
            }
            Keysym::Tab => {
                self.input_method.commit_string("\t".to_string());
                self.serial += 1;
                self.input_method.commit(self.serial);
                return;
            }
            Keysym::Escape => {
                // Escape - typically used to cancel, ignore for now
                eprintln!("Escape pressed (ignored)");
                return;
            }
            // Arrow keys, Home, End, etc. - these need virtual keyboard to forward
            // For now, just ignore them (they won't work in text fields)
            Keysym::Left
            | Keysym::Right
            | Keysym::Up
            | Keysym::Down
            | Keysym::Home
            | Keysym::End
            | Keysym::Page_Up
            | Keysym::Page_Down
            | Keysym::Delete => {
                eprintln!("Navigation key {:?} (not yet supported)", keysym);
                return;
            }
            _ => {}
        }

        // If we have a printable character, commit it
        if !utf8.is_empty() && !utf8.chars().all(|c| c.is_control()) {
            eprintln!("[COMMIT] string={:?}", utf8);
            self.input_method.commit_string(utf8);
            self.serial += 1;
            self.input_method.commit(self.serial);
        } else {
            eprintln!("[SKIP] no printable char, ctrl={}", self.ctrl_pressed);
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
                // Don't release keyboard grab here - causes issues when switching windows
                // Grab will be released on exit
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
