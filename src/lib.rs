//! Library target exposing cross-cutting runtime components so they can be
//! unit-tested independently of the `main` binary, and reused by later
//! tasks/specs.
//!
//! `bootstrap` (task 7.4's Composition Root) is exposed here — rather than
//! staying a `main.rs`-local `mod bootstrap;` as it was through task 7.3 —
//! for two reasons: (1) this codebase's established convention (tasks 1-7)
//! is that testable logic lives on the `lib` target, and (2) task 7.4's own
//! integration tests (`tests/bootstrap_lifecycle_it.rs`,
//! `tests/bootstrap_fail_fast_it.rs`) must reach `bootstrap()` and its
//! injectable-shutdown test seam from a separate `tests/*.rs` binary/process
//! — see that file's module doc comment for why process isolation is
//! required here (in short: `telemetry::init_telemetry` installs a global,
//! install-once-per-process `tracing` subscriber, so any test exercising the
//! full startup sequence must not share a process with
//! `telemetry`'s own unit test that deliberately calls it twice). `main.rs`
//! now calls `kawasemi::bootstrap::bootstrap()` instead of declaring its own
//! `mod bootstrap;`.

pub mod actor;
pub mod bootstrap;
pub mod config;
pub mod db;
pub mod domain;
pub mod error;
pub mod migrate;
pub mod runtime;
pub mod server;
pub mod state;
pub mod telemetry;
pub mod test_harness;
