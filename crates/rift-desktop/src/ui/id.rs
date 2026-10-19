//! Stable widget identities for the immediate-mode UI.
//!
//! Every interactive/stateful widget needs an id that survives across frames:
//! hover, press, focus, scroll offsets, text-input cursors and popup state are
//! all keyed by it. Ids are hashes, not pointers, so they are deterministic for
//! the same call site and can be derived from source strings:
//!
//! ```ignore
//! let id = ui.id("titlebar/search");
//! let row_id = ui.id("sidebar").with(&playlist.name); // stable per playlist
//! ```
//!
//! Ids are only guaranteed stable **within one run of the process**; that is all
//! the UI needs (nothing persists ids to disk).

use std::hash::{Hash, Hasher};

/// A widget identity: a 64-bit hash.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(pub u64);

impl Id {
    /// No widget. Distinct from any hash-derived id in practice.
    pub const NONE: Id = Id(0);

    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Hash a stable string into an id.
    ///
    /// Named `from_str` deliberately: it is the UI's id constructor, not a
    /// `std::str::FromStr` implementation (ids are infallible).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Self {
        Self(hash_str(s))
    }

    /// Derive a child id from this one plus any hashable key (index, name, …).
    /// Two different keys always give different child ids.
    pub fn with(self, key: impl Hash) -> Id {
        let mut h = Fnv1a::default();
        h.write_u64(self.0);
        key.hash(&mut h);
        Id(h.finish())
    }

    /// Derive a child id from a numeric index (the common list-row case).
    pub const fn child(self, index: u64) -> Id {
        // Cheap and dependency-free; mixing a splitmix64 step avoids adjacent
        // indices colliding on the low bits of FNV-style parents.
        let mut z = self
            .0
            .wrapping_add(index.wrapping_add(0x9e37_79b9_7f4a_7c15));
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        Id(z ^ (z >> 31))
    }

    pub const fn raw(self) -> u64 {
        self.0
    }

    /// A raw id, for hashing structs into `Id`s by hand.
    pub fn hash_of(key: impl Hash) -> Id {
        let mut h = Fnv1a::default();
        key.hash(&mut h);
        Id(h.finish())
    }
}

impl From<&str> for Id {
    fn from(s: &str) -> Self {
        Id::from_str(s)
    }
}

impl From<u64> for Id {
    fn from(v: u64) -> Self {
        Id(v)
    }
}

/// FNV-1a 64: deterministic, fast, and good enough for widget ids (we never
/// defend against adversarial collisions).
#[derive(Clone, Copy, Debug)]
struct Fnv1a(u64);

impl Default for Fnv1a {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv1a {
    fn write_u64(&mut self, v: u64) {
        for b in v.to_le_bytes() {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn finish(self) -> u64 {
        self.0
    }
}

impl Hasher for Fnv1a {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

/// Hash a string into a stable 64-bit value.
pub fn hash_str(s: &str) -> u64 {
    let mut h = Fnv1a::default();
    h.write(s.as_bytes());
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_ids_are_stable_and_distinct() {
        assert_eq!(Id::from_str("sidebar/home"), Id::from_str("sidebar/home"));
        assert_ne!(
            Id::from_str("sidebar/home"),
            Id::from_str("sidebar/settings")
        );
        assert_ne!(Id(hash_str("a")), Id(hash_str("b")));
    }

    #[test]
    fn children_do_not_collide_for_neighbours() {
        let base = Id::from_str("track-list");
        let ids: Vec<Id> = (0..1000).map(|i| base.child(i)).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "child ids must be unique");
    }

    #[test]
    fn with_is_sensitive_to_key_type_and_value() {
        let base = Id::from_str("menu");
        assert_eq!(base.with("play"), base.with("play"));
        assert_ne!(base.with("play"), base.with("pause"));
        assert_ne!(base.with(1u64), base.with(1u32).with(0u8));
        // Derivation is deterministic.
        assert_eq!(base.with(7u64), base.with(7u64));
    }
}
