//! Wayland protocol state
//!
//! Manages Wayland protocol handles, serial numbers, and activation state.

use std::os::fd::{AsFd, FromRawFd, OwnedFd};

use wayland_client::QueueHandle;
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2,
    zwp_input_method_v2::ZwpInputMethodV2,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

use crate::State;

/// Wayland protocol state
pub struct WaylandState {
    /// Queue handle for creating new protocol objects
    pub qh: QueueHandle<State>,
    /// Input method protocol instance
    pub input_method: ZwpInputMethodV2,
    /// Active keyboard grab (when IME is enabled)
    pub keyboard_grab: Option<ZwpInputMethodKeyboardGrabV2>,
    /// Protocol serial number for commits
    pub serial: u32,
    /// Whether IME is active (text field focused)
    pub active: bool,
    /// Virtual keyboard for clearing stuck modifier state after grab release
    pub virtual_keyboard: Option<ZwpVirtualKeyboardV1>,
    /// Whether the virtual keyboard has a keymap set (required before sending events)
    pub virtual_keyboard_ready: bool,
    /// Pending activate flag (set in Activate, processed in Done)
    pub pending_activate: bool,
    /// Pending deactivate flag (set in Deactivate, processed in Done)
    pub pending_deactivate: bool,
}

impl WaylandState {
    /// Create new Wayland state
    pub fn new(qh: QueueHandle<State>, input_method: ZwpInputMethodV2) -> Self {
        Self {
            qh,
            input_method,
            keyboard_grab: None,
            serial: 0,
            active: false,
            virtual_keyboard: None,
            virtual_keyboard_ready: false,
            pending_activate: false,
            pending_deactivate: false,
        }
    }

    /// Grab the keyboard for input processing
    pub fn grab_keyboard(&mut self) -> bool {
        if self.keyboard_grab.is_some() {
            return false;
        }
        let grab = self.input_method.grab_keyboard(&self.qh, ());
        self.keyboard_grab = Some(grab);
        true
    }

    /// Release the keyboard grab
    pub fn release_keyboard(&mut self) -> bool {
        if let Some(grab) = self.keyboard_grab.take() {
            grab.release();
            self.clear_modifiers();
            true
        } else {
            false
        }
    }

    /// Set the keymap on the virtual keyboard (must be called before clear_modifiers)
    pub fn set_virtual_keymap(&mut self, keymap_str: &str) {
        if let Some(ref vk) = self.virtual_keyboard
            && let Some(fd) = create_keymap_memfd(keymap_str)
        {
            let size = (keymap_str.len() + 1) as u32; // +1 for null terminator
            vk.keymap(1, fd.as_fd(), size); // 1 = XKB_V1 format
            self.virtual_keyboard_ready = true;
            log::debug!("[VK] Keymap set on virtual keyboard (size={})", size);
        }
    }

    /// Clear all modifier state via virtual keyboard.
    /// This fixes stuck modifiers (e.g., Alt from toggle keybind leaking to the app
    /// before the keyboard grab starts, then the release being consumed by the grab).
    pub fn clear_modifiers(&self) {
        if self.virtual_keyboard_ready
            && let Some(ref vk) = self.virtual_keyboard
        {
            vk.modifiers(0, 0, 0, 0);
            log::debug!("[VK] Cleared modifiers via virtual keyboard");
        }
    }

    /// Update preedit and commit
    pub fn set_preedit(&mut self, text: &str, cursor_begin: i32, cursor_end: i32) {
        self.input_method
            .set_preedit_string(text.to_string(), cursor_begin, cursor_end);
        self.input_method.commit(self.serial);
    }

    /// Commit text to the application
    pub fn commit_string(&mut self, text: &str) {
        self.input_method.commit_string(text.to_string());
        self.input_method.set_preedit_string(String::new(), 0, 0);
        self.input_method.commit(self.serial);
    }

    /// Delete surrounding text
    pub fn delete_surrounding(&mut self, before: u32, after: u32) {
        self.input_method.delete_surrounding_text(before, after);
        self.input_method.commit(self.serial);
    }

    /// Send a key event via the virtual keyboard (for passthrough).
    /// Sends modifiers, key press, key release, then clears modifiers.
    pub fn send_virtual_key(
        &self,
        keycode: u32,
        mods_depressed: u32,
        mods_latched: u32,
        mods_locked: u32,
        mods_group: u32,
    ) {
        if !self.virtual_keyboard_ready {
            log::warn!("[VK] Cannot send virtual key — keymap not set");
            return;
        }
        let Some(ref vk) = self.virtual_keyboard else {
            log::warn!("[VK] Cannot send virtual key — no virtual keyboard");
            return;
        };
        // Set current modifier state
        vk.modifiers(mods_depressed, mods_latched, mods_locked, mods_group);
        // Key press (time=0 is fine for synthetic events)
        vk.key(0, keycode, 1); // 1 = pressed
        // Key release
        vk.key(0, keycode, 0); // 0 = released
        // Clear modifiers after the key event
        vk.modifiers(0, 0, 0, 0);
        log::debug!(
            "[VK] Sent virtual key: keycode={}, mods_depressed=0x{:x}",
            keycode,
            mods_depressed
        );
    }
}

/// Create a memfd containing the keymap string (with null terminator) for the virtual keyboard
fn create_keymap_memfd(keymap_str: &str) -> Option<OwnedFd> {
    use std::io::{Seek, Write};

    let fd = unsafe { libc::memfd_create(c"vk-keymap".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        log::error!("[VK] memfd_create failed");
        return None;
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(keymap_str.as_bytes()).ok()?;
    file.write_all(&[0]).ok()?; // null terminator
    file.seek(std::io::SeekFrom::Start(0)).ok()?;
    Some(OwnedFd::from(file))
}
