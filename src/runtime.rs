//! Runtime injection boundaries (`RuntimeContext` and its constituent
//! non-determinism boundaries), Requirements 5.1-5.6.
//!
//! This module will eventually aggregate the four injection boundaries
//! (clock / id / rng / signing key) behind `RuntimeContext` (design.md's
//! "RuntimeContext と注入境界"), but at this task only the `clock`
//! boundary (Requirement 5.1) is implemented. The remaining boundaries
//! (`ids`, `rng`, `signing_key`) and the `RuntimeContext` aggregate itself
//! are added by later tasks in this same task group.

pub mod clock;

pub use clock::{Clock, FixedClock, SystemClock};
