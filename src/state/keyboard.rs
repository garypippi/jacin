//! Keyboard state management
//!
//! Handles XKB keymap, modifier tracking, and key debouncing.

use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Instant;
use xkbcommon::xkb;

use crate::keysym::KeyMods;

/// Raw modifier state from the keyboard grab (for virtual keyboard passthrough)
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Modifiers {
    pub depressed: u32,
    pub latched: u32,
    pub locked: u32,
    pub group: u32,
}

/// Keyboard state including XKB and modifier tracking
pub struct KeyboardState {
    pub xkb_context: xkb::Context,
    /// Current XKB state (after keymap loaded)
    pub xkb_state: Option<xkb::State>,
    /// Keys that should be ignored (pressed before we were ready)
    pub ignored_keys: HashSet<u32>,
    /// Time when we became ready (for debouncing)
    pub ready_time: Option<Instant>,
    /// Whether we're waiting for a keymap after grab
    pub pending_keymap: bool,
    /// Key repeat rate (events/sec, 0 = disabled)
    pub repeat_rate: i32,
    /// Key repeat initial delay (ms)
    pub repeat_delay: i32,
    /// Hash of the last successfully loaded keymap (for caching)
    pub keymap_hash: Option<u64>,
    /// Whether this is a reactivation (skip debounce)
    pub is_reactivation: bool,
    pub mods: Modifiers,
    /// Keys pressed since the grab started (a release of any other key
    /// belongs to the application)
    pressed_keys: HashSet<u32>,
}

impl KeyboardState {
    pub fn new() -> Self {
        Self {
            xkb_context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            xkb_state: None,
            ignored_keys: HashSet::new(),
            ready_time: None,
            pending_keymap: false,
            repeat_rate: 0,
            repeat_delay: 0,
            keymap_hash: None,
            is_reactivation: false,
            mods: Modifiers::default(),
            pressed_keys: HashSet::new(),
        }
    }

    /// Load keymap from string (skips XKB reload if hash matches)
    pub fn load_keymap(&mut self, keymap_str: &str) -> bool {
        let mut hasher = DefaultHasher::new();
        keymap_str.hash(&mut hasher);
        let hash = hasher.finish();

        if self.keymap_hash == Some(hash) {
            log::debug!("Keymap unchanged (cache hit, skipped XKB reload)");
            self.pending_keymap = false;
            return true;
        }

        if let Some(keymap) = xkb::Keymap::new_from_string(
            &self.xkb_context,
            keymap_str.to_string(),
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        ) {
            self.xkb_state = Some(xkb::State::new(&keymap));
            self.pending_keymap = false;
            self.keymap_hash = Some(hash);
            true
        } else {
            false
        }
    }

    pub fn update_modifiers(
        &mut self,
        mods_depressed: u32,
        mods_latched: u32,
        mods_locked: u32,
        group: u32,
    ) {
        self.mods = Modifiers {
            depressed: mods_depressed,
            latched: mods_latched,
            locked: mods_locked,
            group,
        };

        if let Some(xkb_state) = &mut self.xkb_state {
            xkb_state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
        }
    }

    /// Check if a key should be ignored (pressed before ready or during debounce)
    pub fn should_ignore_key(&mut self, key: u32) -> bool {
        if self.pending_keymap {
            self.ignored_keys.insert(key);
            return true;
        }

        if self.ignored_keys.contains(&key) {
            return true;
        }

        // Check debounce window (200ms after ready)
        if let Some(ready_time) = self.ready_time {
            if ready_time.elapsed().as_millis() < 200 {
                self.ignored_keys.insert(key);
                return true;
            }
            self.ready_time = None;
        }

        false
    }

    /// Handle key release - remove from ignored set
    pub fn handle_key_release(&mut self, key: u32) {
        self.ignored_keys.remove(&key);
    }

    /// Mark as ready with debounce window (skipped on reactivation)
    pub fn mark_ready(&mut self) {
        if self.is_reactivation {
            // Skip debounce — this is a window switch, not a fresh enable
            self.is_reactivation = false;
        } else {
            self.ready_time = Some(Instant::now());
        }
    }

    /// Modifiers held (pressed or latched), looked up by name in the keymap
    pub fn key_mods(&self) -> KeyMods {
        let Some(state) = self.xkb_state.as_ref() else {
            return KeyMods::default();
        };
        let held = |name: &str| {
            state.mod_name_is_active(name, xkb::STATE_MODS_DEPRESSED | xkb::STATE_MODS_LATCHED)
        };
        KeyMods {
            ctrl: held(xkb::MOD_NAME_CTRL),
            alt: held(xkb::MOD_NAME_ALT),
            shift: held(xkb::MOD_NAME_SHIFT),
            logo: held(xkb::MOD_NAME_LOGO),
        }
    }

    /// Get keysym and UTF-8 for a key
    pub fn get_key_info(&self, key: u32) -> Option<(xkb::Keysym, String)> {
        let xkb_state = self.xkb_state.as_ref()?;
        let keycode = xkb::Keycode::new(key + 8); // evdev to xkb
        let keysym = xkb_state.key_get_one_sym(keycode);
        let utf8 = xkb_state.key_get_utf8(keycode);
        Some((keysym, utf8))
    }

    /// Record a key press or release from the grab. Returns false for a
    /// release whose press came before the grab.
    pub fn track_key(&mut self, key: u32, pressed: bool) -> bool {
        if pressed {
            self.pressed_keys.insert(key);
            true
        } else {
            self.pressed_keys.remove(&key)
        }
    }

    /// Reset modifier and pressed-key state (call when releasing keyboard grab)
    pub fn reset_modifiers(&mut self) {
        self.pressed_keys.clear();
        self.mods = Modifiers::default();
    }

    pub fn set_repeat_info(&mut self, rate: i32, delay: i32) {
        self.repeat_rate = rate;
        self.repeat_delay = delay;
    }

    /// Whether a key is a modifier (Shift, Control, Alt, Super, AltGr, ...)
    pub fn is_modifier(&self, key: u32) -> bool {
        self.xkb_state.as_ref().is_some_and(|state| {
            let keycode = xkb::Keycode::new(key + 8);
            is_modifier_keysym(state.key_get_one_sym(keycode).raw())
        })
    }

    /// Check if a key should repeat according to XKB keymap
    pub fn key_repeats(&self, key: u32) -> bool {
        self.xkb_state.as_ref().is_some_and(|state| {
            let keycode = xkb::Keycode::new(key + 8);
            state.get_keymap().key_repeats(keycode)
        })
    }
}

/// Modifier keysyms: Shift_L..Hyper_R, the ISO level/group shifts and
/// locks, Mode_switch
fn is_modifier_keysym(raw: u32) -> bool {
    matches!(raw, 0xffe1..=0xffee | 0xfe01..=0xfe13 | 0xff7e)
}

impl Default for KeyboardState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_keysyms() {
        for sym in [
            xkb::Keysym::Alt_L,
            xkb::Keysym::Control_R,
            xkb::Keysym::Super_L,
            xkb::Keysym::Shift_L,
            xkb::Keysym::ISO_Level3_Shift,
        ] {
            assert!(is_modifier_keysym(sym.raw()), "{sym:?}");
        }
        for sym in [xkb::Keysym::Return, xkb::Keysym::q, xkb::Keysym::Escape] {
            assert!(!is_modifier_keysym(sym.raw()), "{sym:?}");
        }
    }
}
