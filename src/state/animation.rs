//! Lightweight animation infrastructure.
//!
//! Provides a thin abstraction to centralise timer-driven visual updates
//! (currently REC-dot blink) behind a uniform `update(now) -> changed` API.
//! Future animations (cursor blink, fade-outs, …) can be added here without
//! touching the main-loop timer wiring.

use std::time::{Duration, Instant};

// ── RecBlink ────────────────────────────────────────────────────────────────

/// Half-cycle duration for the REC indicator blink (visible or hidden).
pub const REC_BLINK_INTERVAL: Duration = Duration::from_millis(500);

/// Blink animation for the recording indicator dot.
///
/// When a recording register is active the dot alternates between visible
/// and hidden every [`REC_BLINK_INTERVAL`].  When recording stops the
/// state resets so the next recording starts with the dot visible.
#[derive(Debug)]
pub struct RecBlink {
    /// Whether the dot is currently visible.
    pub on: bool,
    /// Timestamp of the last toggle (None when idle / reset).
    last_toggle: Option<Instant>,
}

impl RecBlink {
    pub fn new() -> Self {
        Self {
            on: true,
            last_toggle: None,
        }
    }

    /// Tick the blink.  `recording` indicates whether a macro register is
    /// currently active.  Returns `true` when the visible state changed.
    pub fn update(&mut self, now: Instant, recording: bool) -> bool {
        if !recording {
            // Reset so next recording starts visible.
            if !self.on || self.last_toggle.is_some() {
                self.on = true;
                self.last_toggle = None;
            }
            return false;
        }

        let last = self.last_toggle.get_or_insert(now);
        if now.duration_since(*last) >= REC_BLINK_INTERVAL {
            self.on = !self.on;
            self.last_toggle = Some(now);
            true
        } else {
            false
        }
    }
}

// ── Animations (aggregate) ──────────────────────────────────────────────────

/// Aggregate of all running animations.
///
/// The main loop calls [`Animations::update_all`] once per tick and
/// re-renders only when it returns `true`.
#[derive(Debug)]
pub struct Animations {
    pub rec_blink: RecBlink,
}

impl Animations {
    pub fn new() -> Self {
        Self {
            rec_blink: RecBlink::new(),
        }
    }

    /// Advance every animation by one tick.  Returns `true` when any visual
    /// state changed and the popup needs a repaint.
    ///
    /// `recording` — the current macro register string (empty = not recording).
    pub fn update_all(&mut self, now: Instant, recording: &str) -> bool {
        let mut changed = false;
        changed |= self.rec_blink.update(now, !recording.is_empty());
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── RecBlink unit tests ─────────────────────────────────────────────

    #[test]
    fn no_toggle_when_not_recording() {
        let mut b = RecBlink::new();
        let now = Instant::now();
        assert!(b.on);
        assert!(!b.update(now, false));
        assert!(b.on);
    }

    #[test]
    fn toggles_after_interval() {
        let mut b = RecBlink::new();
        let t0 = Instant::now();

        // First call initialises timestamp, no toggle.
        assert!(!b.update(t0, true));
        assert!(b.on);

        // Backdate to simulate elapsed time.
        b.last_toggle = Some(t0 - REC_BLINK_INTERVAL - Duration::from_millis(1));
        assert!(b.update(t0, true));
        assert!(!b.on);
    }

    #[test]
    fn resets_on_recording_stop() {
        let mut b = RecBlink::new();
        let t0 = Instant::now();

        // Start and force a toggle off.
        b.update(t0, true);
        b.last_toggle = Some(t0 - REC_BLINK_INTERVAL - Duration::from_millis(1));
        b.update(t0, true);
        assert!(!b.on);

        // Stop recording — resets to visible.
        b.update(t0, false);
        assert!(b.on);
        assert!(b.last_toggle.is_none());
    }

    #[test]
    fn starts_visible_on_new_recording() {
        let mut b = RecBlink::new();
        let t0 = Instant::now();

        // First recording: toggle off.
        b.update(t0, true);
        b.last_toggle = Some(t0 - REC_BLINK_INTERVAL - Duration::from_millis(1));
        b.update(t0, true);
        assert!(!b.on);

        // Stop, then start new recording.
        b.update(t0, false);
        assert!(b.on); // visible on fresh start
    }

    // ── Animations aggregate ────────────────────────────────────────────

    #[test]
    fn update_all_propagates_change() {
        let mut a = Animations::new();
        let t0 = Instant::now();

        // Not recording → no change.
        assert!(!a.update_all(t0, ""));

        // Start recording, first tick initialises only.
        assert!(!a.update_all(t0, "q"));

        // Backdate and tick again.
        a.rec_blink.last_toggle = Some(t0 - REC_BLINK_INTERVAL - Duration::from_millis(1));
        assert!(a.update_all(t0, "q"));
        assert!(!a.rec_blink.on);
    }
}
