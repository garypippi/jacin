//! Wayland protocol state
//!
//! Manages Wayland protocol handles, serial numbers, and activation state.

use wayland_client::QueueHandle;
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2,
    zwp_input_method_v2::ZwpInputMethodV2,
};

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
        }
    }

    /// Increment serial and return the new value
    pub fn next_serial(&mut self) -> u32 {
        self.serial += 1;
        self.serial
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
            true
        } else {
            false
        }
    }

    /// Update preedit and commit
    pub fn set_preedit(&mut self, text: &str, cursor_begin: i32, cursor_end: i32) {
        self.input_method
            .set_preedit_string(text.to_string(), cursor_begin, cursor_end);
        let serial = self.next_serial();
        self.input_method.commit(serial);
    }

    /// Commit text to the application
    pub fn commit_string(&mut self, text: &str) {
        self.input_method.commit_string(text.to_string());
        self.input_method.set_preedit_string(String::new(), 0, 0);
        let serial = self.next_serial();
        self.input_method.commit(serial);
    }

    /// Delete surrounding text
    pub fn delete_surrounding(&mut self, before: u32, after: u32) {
        self.input_method.delete_surrounding_text(before, after);
        let serial = self.next_serial();
        self.input_method.commit(serial);
    }

    /// Clear preedit without committing text
    pub fn clear_preedit(&mut self) {
        self.input_method.set_preedit_string(String::new(), -1, -1);
        let serial = self.next_serial();
        self.input_method.commit(serial);
    }
}
