//! Keys sent to Neovim and not yet acknowledged
//!
//! Every key sent to Neovim carries a sequence number that Neovim echoes in
//! `KeyProcessed` and `PassthroughKey`, so a message that arrives late (after
//! the wait for its key timed out) is still matched to its own key and never
//! taken for the next one's.

use std::collections::VecDeque;

use super::Modifiers;

/// A key as it was pressed, kept to pass it through to the application
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SentKey {
    /// Raw evdev keycode
    pub keycode: u32,
    /// Modifiers at the time of the press (they may have changed by the time
    /// a late passthrough arrives)
    pub mods: Modifiers,
}

#[derive(Default)]
pub struct SentKeys {
    next_seq: u64,
    /// Unacknowledged keys in sending order
    keys: VecDeque<(u64, SentKey)>,
}

impl SentKeys {
    /// Register a key about to be sent; returns its sequence number
    pub fn push(&mut self, key: SentKey) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.keys.push_back((seq, key));
        seq
    }

    pub fn get(&self, seq: u64) -> Option<SentKey> {
        self.keys
            .iter()
            .find(|(s, _)| *s == seq)
            .map(|(_, key)| *key)
    }

    /// Forget the key `seq` and all keys sent before it (Neovim processes
    /// keys in order, so they are done too)
    pub fn acknowledge(&mut self, seq: u64) {
        while self.keys.front().is_some_and(|(s, _)| *s <= seq) {
            self.keys.pop_front();
        }
    }

    /// Forget a key that could not be sent
    pub fn remove(&mut self, seq: u64) {
        self.keys.retain(|(s, _)| *s != seq);
    }

    /// Forget all keys (Neovim exited: they will never be acknowledged)
    pub fn clear(&mut self) {
        self.keys.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(keycode: u32) -> SentKey {
        SentKey {
            keycode,
            mods: Modifiers::default(),
        }
    }

    #[test]
    fn sequence_numbers_increase() {
        let mut keys = SentKeys::default();
        assert_eq!(keys.push(key(1)), 0);
        assert_eq!(keys.push(key(2)), 1);
        assert_eq!(keys.get(0), Some(key(1)));
        assert_eq!(keys.get(1), Some(key(2)));
    }

    #[test]
    fn acknowledge_forgets_earlier_keys() {
        let mut keys = SentKeys::default();
        let a = keys.push(key(1));
        let b = keys.push(key(2));
        let c = keys.push(key(3));

        keys.acknowledge(b);
        assert_eq!(keys.get(a), None);
        assert_eq!(keys.get(b), None);
        assert_eq!(keys.get(c), Some(key(3)));
    }

    #[test]
    fn remove_and_clear() {
        let mut keys = SentKeys::default();
        let a = keys.push(key(1));
        let b = keys.push(key(2));

        keys.remove(a);
        assert_eq!(keys.get(a), None);
        assert_eq!(keys.get(b), Some(key(2)));

        keys.clear();
        assert_eq!(keys.get(b), None);
        // Sequence numbers are not reused
        assert_eq!(keys.push(key(3)), 2);
    }
}
