//! Shared, cross-spec canonical domain primitives (DomainPrimitives
//! boundary, Requirements 9.1-9.4).
//!
//! Scope: this module owns the single canonical definition of the
//! lightweight value types shared by every downstream feature spec
//! (accounts-and-instance / statuses-core / social-graph / notifications):
//! the [`primitives::Id`] identifier, the [`primitives::AccountRef`]
//! local/remote actor reference, and the [`primitives::Visibility`] post
//! visibility enum. Downstream specs import these rather than redefining
//! them (Requirement 9.4).
//!
//! This module owns no behavior: it does not decide *what* a visibility
//! value permits (`VisibilityPolicy`, owned by statuses-core per
//! Requirement 9.3), does not know about the `Account` entity or ownership
//! metadata behind an `AccountRef` (Requirement 9.1), and does not generate
//! `Id` values itself (that is `IdGenerator`, task 5.3's `runtime::ids`
//! component) — it only defines the shared representation those
//! components build on.

pub mod primitives;

pub use primitives::{AccountRef, Id, Visibility};
