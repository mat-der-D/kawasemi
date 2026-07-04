//! Canonical shared value types: `Id`, `AccountRef`, `Visibility`
//! (DomainPrimitives boundary, Requirements 9.1-9.4).
//!
//! These are pure, behavior-free value types with no dependency on any
//! entity module (`Account`, `Status`, ...): they carry only the shape and
//! serialization convention that downstream specs standardize on, never
//! ownership metadata or policy. Concretely:
//!
//! - [`Id`] is the single canonical identifier representation for every
//!   domain entity across specs. Its internal representation is a 64-bit
//!   signed integer, monotonically increasing in generation-time order (a
//!   Snowflake-style value payload out by `IdGenerator::next_id()`, task
//!   5.3's `runtime::ids` component — not defined here). Its serde
//!   representation is a decimal string rather than a JSON number, to avoid
//!   JSON's 53-bit safe-integer ceiling and to match the convention of
//!   Mastodon-compatible APIs representing IDs as strings. Its database
//!   column type is `BIGINT`; callers move between `Id` and the raw `i64`
//!   sqlx binds/reads via [`Id::as_i64`]/[`Id::from_i64`] (there is no
//!   direct `sqlx::Type` impl on `Id` itself — see design.md's
//!   DomainPrimitives Implementation Notes).
//! - [`AccountRef`] distinguishes a local actor from a remote one by `Id`
//!   alone. It exposes no owner information and has no knowledge of the
//!   `Account` entity (Requirement 9.1).
//! - [`Visibility`] enumerates the four post visibility levels and fixes
//!   their serde/string representation only. It deliberately owns no
//!   policy/behavior (e.g. "who may see a `Private` post") — that belongs
//!   to statuses-core's `VisibilityPolicy` (Requirement 9.3).

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Canonical identifier shared by every domain entity across specs.
///
/// Sortable by generation time (monotonically increasing), but makes no
/// cryptographic-randomness or unguessability guarantee — callers that need
/// that property must provide it themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(i64);

impl Id {
    /// Wraps a raw `i64` (e.g. a `BIGINT` column value already read from
    /// the database, or a value produced by `IdGenerator::next_id()`) as an
    /// `Id`.
    pub fn from_i64(raw: i64) -> Self {
        Self(raw)
    }

    /// Returns the raw `i64` representation, e.g. to bind as a `BIGINT`
    /// query parameter.
    pub fn as_i64(&self) -> i64 {
        self.0
    }
}

/// Serializes as a decimal string (e.g. `12345` -> `"12345"`), not a JSON
/// number, so large `Id` values survive round trips through JSON's 53-bit
/// safe-integer ceiling and match Mastodon-compatible APIs' string IDs.
impl Serialize for Id {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

/// Deserializes from the same decimal string representation `Serialize`
/// produces; any string that does not parse as an `i64` is rejected.
impl<'de> Deserialize<'de> for Id {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        raw.parse::<i64>().map(Id).map_err(|err| {
            serde::de::Error::custom(format!(
                "invalid Id decimal string {raw:?}: {err}"
            ))
        })
    }
}

/// Distinguishes a local actor from a remote one, by [`Id`] alone.
///
/// Deliberately carries no owner information and has no knowledge of the
/// `Account` entity (Requirement 9.1) — it is a pure reference, not a
/// lookup or a handle with attached metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountRef {
    Local(Id),
    Remote(Id),
}

/// A post's visibility level.
///
/// Owns only the enum and its serde/string representation (Requirement
/// 9.2); it deliberately owns no visibility *policy* (e.g. who may see a
/// `Private` post) — that behavior belongs to statuses-core's
/// `VisibilityPolicy` (Requirement 9.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Unlisted,
    Private,
    Direct,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_round_trips_through_its_decimal_string_serde_representation() {
        let id = Id::from_i64(9_223_372_036_854_775_807);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"9223372036854775807\"");
        let back: Id = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
        assert_eq!(back.as_i64(), id.as_i64());
    }

    #[test]
    fn id_round_trips_negative_and_zero_values_too() {
        // `Id` wraps a *signed* 64-bit integer (design.md's DomainPrimitives
        // Service Interface): the serde/DB round trip must not silently
        // assume non-negativity.
        for raw in [i64::MIN, -1, 0, 1, i64::MAX] {
            let id = Id::from_i64(raw);
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("\"{raw}\""));
            let back: Id = serde_json::from_str(&json).unwrap();
            assert_eq!(back.as_i64(), raw);
        }
    }

    #[test]
    fn id_deserialize_rejects_a_non_decimal_string() {
        let result: Result<Id, _> = serde_json::from_str("\"not-a-number\"");
        assert!(result.is_err());
    }

    #[test]
    fn id_as_i64_and_from_i64_round_trip_the_db_bigint_representation() {
        let raw = 42i64;
        assert_eq!(Id::from_i64(raw).as_i64(), raw);
    }

    #[test]
    fn visibility_serde_string_representation_is_stable_per_variant() {
        let cases = [
            (Visibility::Public, "\"public\""),
            (Visibility::Unlisted, "\"unlisted\""),
            (Visibility::Private, "\"private\""),
            (Visibility::Direct, "\"direct\""),
        ];
        for (variant, expected_json) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json);
            let back: Visibility = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn account_ref_distinguishes_local_from_remote_with_the_same_id() {
        let id = Id::from_i64(42);
        let local = AccountRef::Local(id);
        let remote = AccountRef::Remote(id);
        assert_ne!(local, remote);
        match local {
            AccountRef::Local(inner) => assert_eq!(inner, id),
            AccountRef::Remote(_) => panic!("expected Local"),
        }
        match remote {
            AccountRef::Remote(inner) => assert_eq!(inner, id),
            AccountRef::Local(_) => panic!("expected Remote"),
        }
    }
}
