//! Wayland protocol state
//!
//! Manages Wayland protocol handles, serial numbers, and activation state.

use std::os::fd::{AsFd, FromRawFd, OwnedFd};

use wayland_client::{QueueHandle, WEnum};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    ContentHint, ContentPurpose,
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2,
    zwp_input_method_v2::ZwpInputMethodV2,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

use crate::State;
use crate::state::SentKey;

/// Wayland protocol state
pub struct WaylandState {
    /// Queue handle for creating new protocol objects
    pub qh: QueueHandle<State>,
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
    /// Hash of the keymap currently set on the virtual keyboard
    virtual_keymap_hash: u64,
    /// Pending activate flag (set in Activate, processed in Done)
    pub pending_activate: bool,
    /// Pending deactivate flag (set in Deactivate, processed in Done)
    pub pending_deactivate: bool,
    /// Content type received since the last Done
    pub pending_content_type: Option<ContentType>,
    /// Whether the focused text field takes a password: keys then go to
    /// the application without passing through jacin or Neovim
    pub sensitive: bool,
}

/// Longest commit_string text the protocol allows
const MAX_COMMIT_BYTES: usize = 4000;

/// Split `text` into pieces of at most `max` bytes on char boundaries
fn utf8_chunks(text: &str, max: usize) -> Vec<&str> {
    let mut chunks = Vec::new();
    let mut rest = text;
    while rest.len() > max {
        let mut end = max;
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (chunk, tail) = rest.split_at(end);
        chunks.push(chunk);
        rest = tail;
    }
    if !rest.is_empty() {
        chunks.push(rest);
    }
    chunks
}

pub type ContentType = (WEnum<ContentHint>, WEnum<ContentPurpose>);

/// A password or PIN field, or one the application marks as sensitive
pub fn is_sensitive((hint, purpose): ContentType) -> bool {
    let secret = ContentHint::SensitiveData | ContentHint::HiddenText;
    let secret_hint = match hint {
        WEnum::Value(hint) => hint.intersects(secret),
        // Hints this version doesn't know alongside the ones it does
        WEnum::Unknown(bits) => bits & secret.bits() != 0,
    };
    secret_hint
        || matches!(
            purpose,
            WEnum::Value(ContentPurpose::Password | ContentPurpose::Pin)
        )
}

impl WaylandState {
    pub fn new(qh: QueueHandle<State>, input_method: ZwpInputMethodV2) -> Self {
        Self {
            qh,
            input_method,
            keyboard_grab: None,
            serial: 0,
            active: false,
            virtual_keyboard: None,
            virtual_keyboard_ready: false,
            virtual_keymap_hash: 0,
            pending_activate: false,
            pending_deactivate: false,
            pending_content_type: None,
            sensitive: false,
        }
    }

    pub fn grab_keyboard(&mut self) -> bool {
        if self.keyboard_grab.is_some() {
            return false;
        }
        let grab = self.input_method.grab_keyboard(&self.qh, ());
        self.keyboard_grab = Some(grab);
        true
    }

    pub fn release_keyboard(&mut self) -> bool {
        if let Some(grab) = self.keyboard_grab.take() {
            grab.release();
            self.clear_modifiers();
            true
        } else {
            false
        }
    }

    /// Set the keymap on the virtual keyboard (must be called before clear_modifiers).
    ///
    /// The grab delivers a keymap on every activation, but setting the same one
    /// again makes the compositor rebuild the virtual keyboard, and the keys it
    /// sends afterwards never reach the application (passthrough silently stops
    /// working once focus has moved between text fields).
    pub fn set_virtual_keymap(&mut self, keymap_str: &str) {
        let hash = hash_keymap(keymap_str);
        if self.virtual_keyboard_ready && self.virtual_keymap_hash == hash {
            log::debug!("[VK] Keymap unchanged, not re-setting");
            return;
        }
        if let Some(ref vk) = self.virtual_keyboard
            && let Some(fd) = create_keymap_memfd(keymap_str)
        {
            let size = (keymap_str.len() + 1) as u32; // +1 for null terminator
            vk.keymap(1, fd.as_fd(), size); // 1 = XKB_V1 format
            self.virtual_keyboard_ready = true;
            self.virtual_keymap_hash = hash;
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

    /// Send an empty preedit to make the application report where the text
    /// cursor is.
    ///
    /// jacin shows the text in its popup and never puts it in the preedit, but
    /// some applications (Firefox and other GTK ones) only report the cursor
    /// rectangle in response to a preedit update. Without this the compositor
    /// keeps placing the popup where the previously focused field was.
    ///
    /// A second commit without a preedit ends the composition the empty
    /// preedit starts: text committed during one is dropped by some editors.
    pub fn refresh_cursor_rectangle(&mut self) {
        if !self.active {
            return;
        }
        self.input_method.set_preedit_string(String::new(), 0, 0);
        self.input_method.commit(self.serial);
        self.input_method.commit(self.serial);
        log::debug!("[IM] Requested cursor rectangle (serial={})", self.serial);
    }

    /// Commit text in pieces of at most `MAX_COMMIT_BYTES`: a longer
    /// request does not fit in a Wayland message and breaks the connection.
    /// Each piece gets its own commit, since a second commit_string before
    /// the commit would replace the first.
    pub fn commit_string(&mut self, text: &str) {
        for chunk in utf8_chunks(text, MAX_COMMIT_BYTES) {
            self.input_method.commit_string(chunk.to_string());
            self.input_method.commit(self.serial);
        }
    }

    /// Release a key the application saw pressed before the grab started
    /// (e.g. Enter that ran `jacin toggle` in a terminal): its release comes
    /// to the grab, and without it the application keeps repeating the key.
    pub fn send_virtual_release(&self, keycode: u32) {
        if !self.virtual_keyboard_ready {
            log::warn!("[VK] Cannot release key {keycode} — keymap not set");
            return;
        }
        if let Some(ref vk) = self.virtual_keyboard {
            vk.key(0, keycode, 0); // 0 = released
            log::debug!("[VK] Released key pressed before the grab: keycode={keycode}");
        }
    }

    /// Send a key event via the virtual keyboard (for passthrough).
    /// Sends modifiers, key press, key release, then clears modifiers.
    pub fn send_virtual_key(&self, key: SentKey) {
        if !self.virtual_keyboard_ready {
            log::warn!("[VK] Cannot send virtual key — keymap not set");
            return;
        }
        let Some(ref vk) = self.virtual_keyboard else {
            log::warn!("[VK] Cannot send virtual key — no virtual keyboard");
            return;
        };
        let mods = key.mods;
        vk.modifiers(mods.depressed, mods.latched, mods.locked, mods.group);
        // Key press (time=0 is fine for synthetic events)
        vk.key(0, key.keycode, 1); // 1 = pressed
        vk.key(0, key.keycode, 0); // 0 = released
        vk.modifiers(0, 0, 0, 0);
        log::debug!(
            "[VK] Sent virtual key: keycode={}, mods_depressed=0x{:x}",
            key.keycode,
            mods.depressed
        );
    }
}

fn hash_keymap(keymap_str: &str) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    keymap_str.hash(&mut hasher);
    hasher.finish()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_chunk() {
        assert_eq!(utf8_chunks("あいう", 4000), vec!["あいう"]);
        assert!(utf8_chunks("", 4000).is_empty());
    }

    #[test]
    fn long_text_splits_on_char_boundaries() {
        // 3-byte chars: 4000 is not a multiple of 3
        let text = "あ".repeat(1500);
        let chunks = utf8_chunks(&text, MAX_COMMIT_BYTES);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 3999);
        assert!(chunks.iter().all(|c| c.len() <= MAX_COMMIT_BYTES));
        assert_eq!(chunks.concat(), text);
    }

    #[test]
    fn exact_limit_is_one_chunk() {
        let text = "a".repeat(MAX_COMMIT_BYTES);
        assert_eq!(utf8_chunks(&text, MAX_COMMIT_BYTES), vec![text.as_str()]);
    }

    fn content(hint: ContentHint, purpose: ContentPurpose) -> ContentType {
        (WEnum::Value(hint), WEnum::Value(purpose))
    }

    #[test]
    fn password_fields_are_sensitive() {
        // What Firefox and Chromium send for <input type="password">
        assert!(is_sensitive(content(
            ContentHint::HiddenText | ContentHint::SensitiveData,
            ContentPurpose::Password
        )));
        assert!(is_sensitive(content(
            ContentHint::None,
            ContentPurpose::Pin
        )));
        assert!(is_sensitive(content(
            ContentHint::SensitiveData,
            ContentPurpose::Normal
        )));
        assert!(is_sensitive((
            WEnum::Unknown(ContentHint::HiddenText.bits() | 0x8000),
            WEnum::Value(ContentPurpose::Normal)
        )));
    }

    #[test]
    fn other_fields_are_not_sensitive() {
        for purpose in [
            ContentPurpose::Normal,
            ContentPurpose::Url,
            ContentPurpose::Terminal,
        ] {
            assert!(!is_sensitive(content(ContentHint::None, purpose)));
        }
        assert!(!is_sensitive(content(
            ContentHint::AutoCapitalization,
            ContentPurpose::Normal
        )));
        assert!(!is_sensitive((WEnum::Unknown(0x8000), WEnum::Unknown(99))));
    }
}
