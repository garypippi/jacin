//! Key repeat state management
//!
//! Tracks timing for key repeat events when a key is held down.

use std::time::Instant;

/// Tracks key repeat progress for a held key
pub struct KeyRepeatState {
    /// evdev keycode currently held for repeat
    key: Option<u32>,
    /// When the key was first pressed
    press_time: Option<Instant>,
    /// Whether we've passed the initial delay
    started: bool,
    /// Last repeat event fire time
    last_fire: Option<Instant>,
}

impl KeyRepeatState {
    pub fn new() -> Self {
        Self {
            key: None,
            press_time: None,
            started: false,
            last_fire: None,
        }
    }

    /// Start tracking a new key press for repeat
    pub fn start(&mut self, key: u32) {
        self.key = Some(key);
        self.press_time = Some(Instant::now());
        self.started = false;
        self.last_fire = None;
    }

    /// Stop repeat for a specific key (on release)
    pub fn stop(&mut self, key: u32) {
        if self.key == Some(key) {
            self.cancel();
        }
    }

    /// Whether a key is currently held for repeat
    pub fn has_key(&self) -> bool {
        self.key.is_some()
    }

    /// Unconditionally cancel all repeat state
    pub fn cancel(&mut self) {
        self.key = None;
        self.press_time = None;
        self.started = false;
        self.last_fire = None;
    }

    /// Check if a repeat event should fire based on compositor rate/delay.
    /// Returns `Some(key)` when it's time to fire, `None` otherwise.
    pub fn should_fire(&mut self, rate: i32, delay: i32) -> Option<u32> {
        if rate <= 0 {
            return None;
        }
        let key = self.key?;
        let press_time = self.press_time?;
        let now = Instant::now();

        if !self.started {
            // Waiting for initial delay
            if press_time.elapsed().as_millis() >= delay as u128 {
                self.started = true;
                self.last_fire = Some(now);
                return Some(key);
            }
        } else {
            // Repeating: check interval (1_000_000µs / rate)
            let interval_us = 1_000_000u64 / rate as u64;
            let last = self.last_fire.unwrap_or(press_time);
            if last.elapsed().as_micros() >= interval_us as u128 {
                self.last_fire = Some(now);
                return Some(key);
            }
        }

        None
    }
}

impl Default for KeyRepeatState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_fire_without_start() {
        let mut state = KeyRepeatState::new();
        assert!(state.should_fire(25, 600).is_none());
    }

    #[test]
    fn no_fire_zero_rate() {
        let mut state = KeyRepeatState::new();
        state.start(42);
        std::thread::sleep(std::time::Duration::from_millis(700));
        assert!(state.should_fire(0, 600).is_none());
    }

    #[test]
    fn fires_after_delay() {
        let mut state = KeyRepeatState::new();
        state.start(42);
        // Should not fire immediately
        assert!(state.should_fire(25, 600).is_none());
        // Wait past delay
        std::thread::sleep(std::time::Duration::from_millis(650));
        assert_eq!(state.should_fire(25, 600), Some(42));
    }

    #[test]
    fn stop_cancels_specific_key() {
        let mut state = KeyRepeatState::new();
        state.start(42);
        state.stop(42);
        std::thread::sleep(std::time::Duration::from_millis(700));
        assert!(state.should_fire(25, 600).is_none());
    }

    #[test]
    fn stop_ignores_different_key() {
        let mut state = KeyRepeatState::new();
        state.start(42);
        state.stop(99); // Different key — no effect
        std::thread::sleep(std::time::Duration::from_millis(650));
        assert_eq!(state.should_fire(25, 600), Some(42));
    }

    #[test]
    fn cancel_stops_all() {
        let mut state = KeyRepeatState::new();
        state.start(42);
        state.cancel();
        std::thread::sleep(std::time::Duration::from_millis(700));
        assert!(state.should_fire(25, 600).is_none());
    }

    #[test]
    fn has_key_tracks_lifecycle() {
        let mut state = KeyRepeatState::new();
        assert!(!state.has_key());

        state.start(10);
        assert!(state.has_key());

        state.stop(99); // different key
        assert!(state.has_key());

        state.stop(10); // tracked key
        assert!(!state.has_key());

        state.start(20);
        assert!(state.has_key());
        state.cancel();
        assert!(!state.has_key());
    }

    #[test]
    fn second_fire_respects_repeat_interval() {
        let mut state = KeyRepeatState::new();
        state.start(7);

        // Wait for initial delay and first fire.
        std::thread::sleep(std::time::Duration::from_millis(620));
        assert_eq!(state.should_fire(20, 600), Some(7)); // 20Hz => 50ms interval

        // Too early for second fire.
        assert!(state.should_fire(20, 600).is_none());

        // Past one interval should fire again.
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert_eq!(state.should_fire(20, 600), Some(7));
    }
}
