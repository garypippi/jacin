use std::os::fd::{AsFd, AsRawFd};

use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum,
    globals::GlobalListContents,
    protocol::{
        wl_buffer, wl_compositor, wl_keyboard, wl_registry, wl_shm, wl_shm_pool, wl_surface,
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
use crate::input::KeyOrigin;

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
                log::info!("IME activated!");

                // Re-grab keyboard if IME was enabled before deactivation.
                // Limit consecutive re-grabs to prevent infinite Deactivate/Activate
                // loops (the grab itself can trigger compositor re-evaluation).
                if state.ime.is_enabled() && state.wayland.keyboard_grab.is_none() {
                    if state.reactivation_count < 2 {
                        state.reactivation_count += 1;
                        log::debug!("[IME] Re-grabbing keyboard after activation (count={})", state.reactivation_count);
                        state.wayland.grab_keyboard();
                        state.keyboard.pending_keymap = true;
                        // false = don't toggle skkeleton (already enabled), just restore insert mode
                        state.ime.start_enabling(false);
                    } else {
                        log::warn!("[IME] Skipping re-grab (too many consecutive reactivations), disabling");
                        state.ime.disable();
                        state.reactivation_count = 0;
                    }
                }
            }
            zwp_input_method_v2::Event::Deactivate => {
                log::info!("IME deactivated");
                state.wayland.active = false;
                // Only do cleanup when IME is enabled — avoids flooding Neovim
                // during rapid compositor activate/deactivate cycles (window switching)
                if state.ime.is_enabled() {
                    // Cancel any active key repeat
                    state.repeat.cancel();
                    // Release keyboard grab to stop receiving key events while deactivated
                    state.wayland.release_keyboard();
                    state.keyboard.reset_modifiers();
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
                            let should_toggle = state.ime.complete_enabling();
                            if should_toggle {
                                // Set ready_time for debouncing
                                state.keyboard.mark_ready();
                                if let Some(ref nvim) = state.nvim {
                                    log::debug!("[IME] Sending skkeleton toggle");
                                    nvim.send_key(&state.config.keybinds.toggle);
                                }
                                // Show icon-only popup immediately
                                state.update_popup();
                            } else if state.ime.is_fully_enabled() {
                                // Re-activation after deactivate/activate cycle:
                                // Neovim is in normal mode from <Esc>ggdG, restore insert mode
                                state.keyboard.mark_ready();
                                if let Some(ref nvim) = state.nvim {
                                    log::debug!("[IME] Restoring insert mode after re-activation");
                                    nvim.send_key("<Esc>i");
                                }
                                // Show icon-only popup immediately
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
                // User interaction: reset reactivation counter
                state.reactivation_count = 0;
                if let WEnum::Value(ks) = key_state {
                    if ks == wl_keyboard::KeyState::Pressed {
                        if state.keyboard.key_repeats(key) {
                            state.repeat.start(key);
                        }
                    } else {
                        state.repeat.stop(key);
                    }
                    state.handle_key(key, ks, KeyOrigin::Physical);
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
