//! Library crate root, re-exporting the core-runtime modules so both the
//! `kawasemi` binary (`src/main.rs`) and integration tests under `tests/`
//! can depend on the same code without duplicating module declarations.

pub mod bootstrap;
pub mod config;
pub mod db;
pub mod migrate;
pub mod telemetry;
