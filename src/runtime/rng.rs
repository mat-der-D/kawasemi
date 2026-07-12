//! `Rng` injection boundary (Requirement 5.3): random-byte generation placed
//! behind an abstract boundary, with a production implementation
//! (Requirement 5.6) backed by the operating system's CSPRNG-grade entropy
//! source, and a deterministic implementation (Requirement 5.5) that
//! reproduces the same byte stream for the same seed so tests stay
//! reproducible.

use std::sync::atomic::{AtomicU64, Ordering};

/// Supplies random bytes, decoupling callers from any concrete entropy
/// source (Requirement 5.3). Implementations must be safe to share across
/// threads (`Send + Sync`) since `RuntimeContext` hands out a single shared
/// instance to concurrent request handlers.
pub trait Rng: Send + Sync {
    fn fill_bytes(&self, buf: &mut [u8]);
}

/// Production `Rng` implementation (Requirement 5.6), backed by the
/// operating system's preferred random number source via [`getrandom`].
/// Implementing raw OS-entropy sourcing by hand would mean reinventing a
/// security-sensitive wheel (correctly sourcing CSPRNG-grade entropy is
/// platform-specific and easy to get subtly wrong), so this defers to the
/// well-regarded `getrandom` crate, which was already resolved transitively
/// in `Cargo.lock` (pulled in by `sqlx-postgres` via `rand`) and is added
/// here as a direct dependency at that already-resolved version.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRng;

impl SystemRng {
    pub fn new() -> Self {
        Self
    }
}

impl Rng for SystemRng {
    fn fill_bytes(&self, buf: &mut [u8]) {
        getrandom::fill(buf).expect("OS entropy source is unavailable");
    }
}

/// Deterministic `Rng` implementation (Requirement 5.5): a seeded xorshift64
/// generator that reproduces the same byte stream for the same seed, so
/// tests can assert against known, reproducible random-looking bytes instead
/// of depending on real, non-reproducible entropy. Determinism, not
/// cryptographic strength, is the only requirement for this test-only path,
/// so a small hand-rolled generator (kept in an [`AtomicU64`] for interior
/// mutability across the shared `&self` calls) is used instead of pulling in
/// a second RNG-family dependency.
#[derive(Debug)]
pub struct SeededRng {
    state: AtomicU64,
}

impl SeededRng {
    pub fn new(seed: u64) -> Self {
        // xorshift64 is undefined for a zero state (it would only ever
        // produce zero), so substitute a fixed nonzero constant in that one
        // case while remaining fully deterministic for the given seed.
        let state = if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        };
        Self {
            state: AtomicU64::new(state),
        }
    }

    /// Advances the shared state by one xorshift64 step and returns the new
    /// value, using a compare-and-swap loop so concurrent callers never
    /// observe or produce a duplicate value.
    fn next_u64(&self) -> u64 {
        let mut current = self.state.load(Ordering::Relaxed);
        loop {
            let mut next = current;
            next ^= next << 13;
            next ^= next >> 7;
            next ^= next << 17;
            match self.state.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return next,
                Err(actual) => current = actual,
            }
        }
    }
}

impl Rng for SeededRng {
    fn fill_bytes(&self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let word = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_rng_reproduces_the_same_byte_sequence_for_the_same_seed() {
        let a = SeededRng::new(42);
        let b = SeededRng::new(42);

        let mut buf_a = [0u8; 37];
        let mut buf_b = [0u8; 37];
        a.fill_bytes(&mut buf_a);
        b.fill_bytes(&mut buf_b);

        assert_eq!(buf_a, buf_b);
        // Sanity check: a real generator shouldn't leave the buffer as all
        // zeroes.
        assert!(buf_a.iter().any(|&byte| byte != 0));
    }

    #[test]
    fn deterministic_rng_reproduces_the_same_sequence_across_multiple_fills() {
        let a = SeededRng::new(7);
        let b = SeededRng::new(7);

        for _ in 0..5 {
            let mut buf_a = [0u8; 16];
            let mut buf_b = [0u8; 16];
            a.fill_bytes(&mut buf_a);
            b.fill_bytes(&mut buf_b);
            assert_eq!(buf_a, buf_b);
        }
    }

    #[test]
    fn deterministic_rng_with_different_seeds_diverges() {
        let a = SeededRng::new(1);
        let b = SeededRng::new(1000);

        let mut buf_a = [0u8; 16];
        let mut buf_b = [0u8; 16];
        a.fill_bytes(&mut buf_a);
        b.fill_bytes(&mut buf_b);

        assert_ne!(buf_a, buf_b);
    }

    #[test]
    fn production_rng_fills_the_whole_buffer_and_is_not_constant() {
        let rng = SystemRng::new();

        let mut first = [0u8; 32];
        let mut second = [0u8; 32];
        rng.fill_bytes(&mut first);
        rng.fill_bytes(&mut second);

        assert_ne!(first, [0u8; 32]);
        assert_ne!(first, second);
    }
}
