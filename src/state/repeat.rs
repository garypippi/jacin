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
