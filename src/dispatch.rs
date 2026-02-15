use std::os::fd::{AsFd, AsRawFd};

use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum,
    globals::GlobalListContents,
    protocol::{
        wl_buffer, wl_compositor, wl_keyboard, wl_region, wl_registry, wl_shm, wl_shm_pool,
        wl_surface,
    },
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2, zwp_input_method_manager_v2, zwp_input_method_v2,
    zwp_input_popup_surface_v2,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1, zwp_virtual_keyboard_v1,
};

use crate::State;
use crate::state::VimMode;

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
            log::debug!("[SHM] Format available: {:?}", format);
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

// Dispatch for region (no events)
impl Dispatch<wl_region::WlRegion, ()> for State {
    fn event(
        _state: &mut Self,
        _region: &wl_region::WlRegion,
        _event: wl_region::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
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
                log::debug!("[SURFACE] Entered output");
            }
            wl_surface::Event::Leave { .. } => {
                log::debug!("[SURFACE] Left output");
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
            log::debug!("[BUFFER] Released: {}", data);
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
            log::debug!(
                "[POPUP] Text input rectangle: x={}, y={}, {}x{}",
                x,
                y,
                width,
                height
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
                log::info!("IME activated!");
                state.wayland.pending_activate = true;
            }
            zwp_input_method_v2::Event::Deactivate => {
                log::info!("IME deactivated");
                state.wayland.pending_deactivate = true;
            }
            zwp_input_method_v2::Event::Done => {
                // Serial must equal the number of Done events received
                // (required by the commit request protocol)
                state.wayland.serial += 1;

                let pending_deactivate = std::mem::take(&mut state.wayland.pending_deactivate);
                let pending_activate = std::mem::take(&mut state.wayland.pending_activate);

                // Process deactivate first (like fcitx5)
                if pending_deactivate {
                    state.wayland.active = false;
                    if state.ime.is_enabled() {
                        // Clear local state (don't send Wayland protocol requests
                        // while deactivated — compositor clears preedit automatically)
                        state.reset_ime_state();
                        // Clear Neovim buffer to reset state for next activation
                        if let Some(ref nvim) = state.nvim {
                            nvim.send_key("<Esc>ggdG");
                        }
                    }
                }

                // Then process activate
                if pending_activate {
                    state.wayland.active = true;
                    if state.ime.is_enabled() && state.wayland.keyboard_grab.is_none() {
                        log::debug!("[IME] Re-grabbing keyboard after activation");
                        state.wayland.grab_keyboard();
                        state.keyboard.pending_keymap = true;
                        state.keyboard.is_reactivation = true;
                        state.ime.start_enabling();
                    }
                }
            }
            zwp_input_method_v2::Event::Unavailable => {
                log::warn!("IME unavailable - another IME may be running");
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
                log::debug!("Keymap received: format={:?}, size={}", format, size);

                if let WEnum::Value(wl_keyboard::KeymapFormat::XkbV1) = format {
                    // Memory-map the keymap (fd is borrowed, we don't own it)
                    let keymap_data =
                        unsafe { memmap_keymap(fd.as_fd().as_raw_fd(), size as usize) };

                    if let Some(data) = keymap_data {
                        // Parse the keymap using KeyboardState
                        if state.keyboard.load_keymap(&data) {
                            log::info!("Keymap loaded successfully");

                            // Set same keymap on virtual keyboard (needed for modifier clearing)
                            state.wayland.set_virtual_keymap(&data);
                            // Clear any stuck modifiers from the toggle keybind
                            // (e.g., Alt leaked to the app before the grab started)
                            state.wayland.clear_modifiers();

                            // Complete enabling if transitioning
                            let initial_mode = if state.config.behavior.startinsert {
                                VimMode::Insert
                            } else {
                                VimMode::Normal
                            };
                            if state.ime.complete_enabling(initial_mode)
                                || state.ime.is_fully_enabled()
                            {
                                // Set vim_mode for popup display to match initial mode
                                if state.config.behavior.startinsert {
                                    state.keypress.set_vim_mode("i");
                                } else {
                                    state.keypress.set_vim_mode("n");
                                }
                                state.keyboard.mark_ready();
                                if let Some(ref nvim) = state.nvim {
                                    if state.config.behavior.startinsert {
                                        log::debug!("[IME] Restoring insert mode");
                                        nvim.send_key("<Esc>i");
                                    } else {
                                        log::debug!("[IME] Restoring normal mode");
                                        nvim.send_key("<Esc>");
                                    }
                                }
                                state.update_popup();
                            }
                        } else {
                            log::error!("Failed to parse keymap");
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
                log::debug!("[GRAB] Key event: key={}, state={:?}", key, key_state);
                if let WEnum::Value(ks) = key_state {
                    if ks == wl_keyboard::KeyState::Pressed {
                        if state.keyboard.key_repeats(key) {
                            state.repeat.start(key);
                        }
                    } else {
                        state.repeat.stop(key);
                        if !state.repeat.has_key() {
                            state.repeat_timer_token = None;
                        }
                    }
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
                log::debug!("Repeat info: rate={}/s, delay={}ms", rate, delay);
                state.keyboard.set_repeat_info(rate, delay);
            }
            _ => {}
        }
    }
}

// Dispatch for virtual keyboard manager (no events)
impl Dispatch<zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, ()> for State {
    fn event(
        _state: &mut Self,
        _manager: &zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
        _event: zwp_virtual_keyboard_manager_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

// Dispatch for virtual keyboard (no events)
impl Dispatch<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1, ()> for State {
    fn event(
        _state: &mut Self,
        _vk: &zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
        _event: zwp_virtual_keyboard_v1::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
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
