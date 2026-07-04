//! Runtime injection boundaries (`RuntimeContext` and its constituent
//! non-determinism boundaries), Requirements 5.1-5.6.
//!
//! This module will eventually aggregate the four injection boundaries
//! (clock / id / rng / signing key) behind `RuntimeContext` (design.md's
//! "RuntimeContext と注入境界"), but so far only the `clock` (Requirement
//! 5.1), `ids` (Requirement 5.2), and `rng` (Requirement 5.3) boundaries are
//! implemented. The remaining boundary (`signing_key`) and the
//! `RuntimeContext` aggregate itself are added by later tasks in this same
//! task group.

pub mod clock;
pub mod ids;
pub mod rng;

pub use clock::{Clock, FixedClock, SystemClock};
pub use ids::{IdGenerator, SeqIdGenerator, SnowflakeIdGenerator};
pub use rng::{Rng, SeededRng, SystemRng};
