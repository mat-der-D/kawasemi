//! `IdGenerator` injection boundary (Requirement 5.2): identifier
//! generation placed behind an abstract boundary, with a production
//! implementation (Requirement 5.6) that packs a millisecond timestamp and
//! an in-process atomic sequence into the canonical `Id`'s 64-bit payload
//! (Snowflake-style, per design.md's DomainPrimitives note that `Id` is
//! "generation-time-ordered monotonically increasing"), and a deterministic
//! implementation (Requirement 5.5) that reproduces the same sequential
//! `Id` sequence for the same seed so tests stay reproducible.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::domain::Id;

/// Supplies newly generated [`Id`] values, decoupling callers from any
/// concrete generation strategy (Requirement 5.2). Implementations must be
/// safe to share across threads (`Send + Sync`) since `RuntimeContext`
/// hands out a single shared instance to concurrent request handlers.
pub trait IdGenerator: Send + Sync {
    fn next_id(&self) -> Id;
}

/// Number of low bits of the 64-bit `Id` payload reserved for the
/// in-process sequence counter; the remaining high bits hold the millisecond
/// timestamp. This only needs to be large enough that a burst of IDs
/// generated within the same millisecond on one process doesn't overflow
/// the sequence and bleed into the timestamp bits under realistic load.
const SEQUENCE_BITS: u32 = 22;

/// Production `IdGenerator` implementation (Requirement 5.6).
///
/// Packs a millisecond-resolution timestamp into the high bits and an
/// atomically incremented sequence counter into the low
/// [`SEQUENCE_BITS`] bits, so IDs generated within the same millisecond
/// still increase monotonically and concurrent calls from multiple threads
/// never collide (the single [`AtomicI64`] counter is shared and updated
/// with one atomic fetch-add per call).
#[derive(Debug, Default)]
pub struct SnowflakeIdGenerator {
    sequence: AtomicI64,
}

impl SnowflakeIdGenerator {
    pub fn new() -> Self {
        Self {
            sequence: AtomicI64::new(0),
        }
    }
}

impl IdGenerator for SnowflakeIdGenerator {
    fn next_id(&self) -> Id {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is set before the Unix epoch")
            .as_millis() as i64;
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) & ((1 << SEQUENCE_BITS) - 1);
        Id::from_i64((millis << SEQUENCE_BITS) | sequence)
    }
}

/// Deterministic `IdGenerator` implementation (Requirement 5.5): starting
/// from a constructed seed, each call to [`next_id`](IdGenerator::next_id)
/// returns the next value in the sequence `seed, seed + 1, seed + 2, ...`
/// via an atomically incremented counter initialized to the seed. Two
/// instances constructed with the same seed always produce the same
/// sequence, so tests can assert against known, reproducible `Id` values
/// instead of depending on wall-clock time or process state.
#[derive(Debug)]
pub struct SeqIdGenerator {
    next: AtomicI64,
}

impl SeqIdGenerator {
    pub fn new(seed: i64) -> Self {
        Self {
            next: AtomicI64::new(seed),
        }
    }
}

impl IdGenerator for SeqIdGenerator {
    fn next_id(&self) -> Id {
        let raw = self.next.fetch_add(1, Ordering::Relaxed);
        Id::from_i64(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_id_generator_reproduces_the_same_sequence_for_the_same_seed() {
        let a = SeqIdGenerator::new(42);
        let b = SeqIdGenerator::new(42);

        let ids_a: Vec<Id> = (0..5).map(|_| a.next_id()).collect();
        let ids_b: Vec<Id> = (0..5).map(|_| b.next_id()).collect();

        assert_eq!(ids_a, ids_b);
        assert_eq!(
            ids_a,
            vec![
                Id::from_i64(42),
                Id::from_i64(43),
                Id::from_i64(44),
                Id::from_i64(45),
                Id::from_i64(46),
            ]
        );
    }

    #[test]
    fn deterministic_id_generator_with_different_seeds_diverges() {
        let a = SeqIdGenerator::new(1);
        let b = SeqIdGenerator::new(1000);

        assert_ne!(a.next_id(), b.next_id());
    }

    #[test]
    fn production_id_generator_produces_distinct_monotonically_increasing_ids() {
        let generator = SnowflakeIdGenerator::new();

        let first = generator.next_id();
        let second = generator.next_id();
        let third = generator.next_id();

        assert!(first.as_i64() < second.as_i64());
        assert!(second.as_i64() < third.as_i64());
    }

    #[test]
    fn production_id_generator_is_safe_for_concurrent_use_and_never_repeats_an_id() {
        use std::collections::HashSet;
        use std::sync::Arc;
        use std::thread;

        let generator = Arc::new(SnowflakeIdGenerator::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let generator = Arc::clone(&generator);
            handles.push(thread::spawn(move || {
                (0..500).map(|_| generator.next_id()).collect::<Vec<_>>()
            }));
        }

        let mut all_ids = HashSet::new();
        for handle in handles {
            for id in handle.join().unwrap() {
                assert!(all_ids.insert(id), "duplicate id generated: {id:?}");
            }
        }
        assert_eq!(all_ids.len(), 8 * 500);
    }
}
