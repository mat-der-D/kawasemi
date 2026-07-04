//! Library target exposing cross-cutting runtime components (starting with
//! `config`) so they can be unit-tested independently of the `main`
//! binary's composition root, and reused by later tasks/specs.
//!
//! `main.rs` remains a standalone binary crate root for now (wiring
//! `config::load_config()` into `bootstrap()` is task 7.4's job, per
//! core-runtime's task boundary for task 2.1) — this file intentionally
//! adds no behavior of its own, only module registration.

pub mod config;
pub mod db;
pub mod domain;
pub mod error;
pub mod migrate;
pub mod runtime;
pub mod server;
pub mod state;
pub mod telemetry;
