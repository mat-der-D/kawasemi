//! Unit tests for the standard database-error conversion.

use super::*;
use crate::error::{ErrorKind, GENERIC_SERVER_MESSAGE};

#[test]
fn maps_to_a_500_server_error_carrying_the_source() {
    let err = map_server_error(sqlx::Error::RowNotFound);

    assert_eq!(err.kind, ErrorKind::Server);
    assert_eq!(err.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        err.source.is_some(),
        "the database failure must survive for the log site"
    );
}

/// The whole point of routing database failures through `AppError::server`
/// is that the query, the table names, and any values bound into it stay
/// out of the response.
#[test]
fn never_surfaces_database_detail_in_the_public_message() {
    let distinctive = "table-name-and-connection-detail-4711";
    let err = map_server_error(sqlx::Error::Protocol(distinctive.to_string()));

    assert_eq!(err.public_message, GENERIC_SERVER_MESSAGE);
    assert!(
        !err.public_message.contains(distinctive),
        "database detail must not reach the caller, got: {}",
        err.public_message
    );
}

#[test]
fn carries_no_error_tag() {
    let err = map_server_error(sqlx::Error::RowNotFound);
    assert_eq!(err.tag, None);
}
