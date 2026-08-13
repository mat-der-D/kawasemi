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

pub mod accounts;
pub mod actor;
pub mod api;
pub mod bootstrap;
pub mod config;
pub mod contract;
pub mod db;
pub mod domain;
pub mod error;
pub mod federation;
pub mod media;
pub mod migrate;
pub mod notifications;
pub mod oauth;
pub mod runtime;
pub mod search;
pub mod server;
pub mod social_graph;
pub mod state;
pub mod statuses;
pub mod telemetry;
// The harness carries fixed test-only credentials (a fixed KEK, a fixed owner
// passphrase, a fixed token-hash key) and a hard-coded test database URL, so it
// must never be compiled into a distributed artifact. Gating the module
// declaration keeps the whole subtree -- including its constants and its
// `sweep`/`reaper`/`db_fixture` children -- out of a plain `cargo build`, while
// `cargo test` (via `test`) and `tests/*` (via the self dev-dependency that
// enables `test-harness`) still see it.
#[cfg(any(test, feature = "test-harness"))]
pub mod test_harness;
pub mod timelines;
