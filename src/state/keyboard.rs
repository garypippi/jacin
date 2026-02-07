//! Keyboard state management
//!
//! Handles XKB keymap, modifier tracking, and key debouncing.

use std::collections::HashSet;
use std::time::Instant;
use xkbcommon::xkb;

/// Keyboard state including XKB and modifier tracking
pub struct KeyboardState {
    /// XKB context for keymap parsing
    pub xkb_context: xkb::Context,
    /// Current XKB state (after keymap loaded)
    pub xkb_state: Option<xkb::State>,
    /// Ctrl modifier pressed
    pub ctrl_pressed: bool,
    /// Alt modifier pressed
    pub alt_pressed: bool,
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
}

impl KeyboardState {
    /// Create new keyboard state
    pub fn new() -> Self {
        Self {
            xkb_context: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            xkb_state: None,
            ctrl_pressed: false,
            alt_pressed: false,
            ignored_keys: HashSet::new(),
            ready_time: None,
            pending_keymap: false,
            repeat_rate: 0,
            repeat_delay: 0,
        }
    }

    /// Load keymap from string
    pub fn load_keymap(&mut self, keymap_str: &str) -> bool {
        if let Some(keymap) = xkb::Keymap::new_from_string(
            &self.xkb_context,
            keymap_str.to_string(),
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        ) {
            self.xkb_state = Some(xkb::State::new(&keymap));
            self.pending_keymap = false;
            true
        } else {
            false
        }
    }

    /// Update modifier state
    pub fn update_modifiers(
        &mut self,
        mods_depressed: u32,
        mods_latched: u32,
        mods_locked: u32,
        group: u32,
    ) {
        const CTRL_MASK: u32 = 0x4;
        const ALT_MASK: u32 = 0x8;

        self.ctrl_pressed = (mods_depressed & CTRL_MASK) != 0;
        self.alt_pressed = (mods_depressed & ALT_MASK) != 0;

        if let Some(xkb_state) = &mut self.xkb_state {
            xkb_state.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
        }
    }

    /// Check if a key should be ignored (pressed before ready or during debounce)
    pub fn should_ignore_key(&mut self, key: u32) -> bool {
        // Check if waiting for keymap
        if self.pending_keymap {
            self.ignored_keys.insert(key);
            return true;
        }

        // Check if in ignored set
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

    /// Mark as ready with debounce window
    pub fn mark_ready(&mut self) {
        self.ready_time = Some(Instant::now());
    }

    /// Get keysym and UTF-8 for a key
    pub fn get_key_info(&self, key: u32) -> Option<(xkb::Keysym, String)> {
        let xkb_state = self.xkb_state.as_ref()?;
        let keycode = xkb::Keycode::new(key + 8); // evdev to xkb
        let keysym = xkb_state.key_get_one_sym(keycode);
        let utf8 = xkb_state.key_get_utf8(keycode);
        Some((keysym, utf8))
    }

    /// Store compositor repeat info
    pub fn set_repeat_info(&mut self, rate: i32, delay: i32) {
        self.repeat_rate = rate;
        self.repeat_delay = delay;
    }

    /// Check if a key should repeat according to XKB keymap
    pub fn key_repeats(&self, key: u32) -> bool {
        self.xkb_state.as_ref().is_some_and(|state| {
            let keycode = xkb::Keycode::new(key + 8);
            state.get_keymap().key_repeats(keycode)
        })
    }
}

impl Default for KeyboardState {
    fn default() -> Self {
        Self::new()
    }
}
