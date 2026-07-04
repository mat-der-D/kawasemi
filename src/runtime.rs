//! Runtime injection boundaries (`RuntimeContext` and its constituent
//! non-determinism boundaries), Requirements 5.1-5.6.
//!
//! This module will eventually aggregate the four injection boundaries
//! (clock / id / rng / signing key) behind `RuntimeContext` (design.md's
//! "RuntimeContext と注入境界"), but so far only the `clock` (Requirement
//! 5.1), `ids` (Requirement 5.2), `rng` (Requirement 5.3), and
//! `signing_key` (Requirement 5.4) boundaries are implemented. The
//! `RuntimeContext` aggregate itself is added by a later task in this same
//! task group.

pub mod clock;
pub mod ids;
pub mod rng;
pub mod signing_key;

pub use clock::{Clock, FixedClock, SystemClock};
pub use ids::{IdGenerator, SeqIdGenerator, SnowflakeIdGenerator};
pub use rng::{Rng, SeededRng, SystemRng};
pub use signing_key::{FixedSigningKeyProvider, KeyError, KeyRef, SigningKey, SigningKeyProvider};
