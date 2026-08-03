use super::*;

#[test]
fn accepts_activity_json_media_type() {
    assert!(accepts_activitypub("application/activity+json"));
}

#[test]
fn accepts_ld_json_media_type() {
    assert!(accepts_activitypub("application/ld+json"));
}

#[test]
fn accepts_ld_json_with_a_profile_parameter() {
    assert!(accepts_activitypub(
        "application/ld+json; profile=\"https://www.w3.org/ns/activitystreams\""
    ));
}

#[test]
fn accepts_within_a_comma_separated_accept_list() {
    assert!(accepts_activitypub(
        "text/html, application/activity+json;q=0.9"
    ));
}

#[test]
fn is_case_insensitive() {
    assert!(accepts_activitypub("APPLICATION/ACTIVITY+JSON"));
}

#[test]
fn rejects_non_activitypub_media_types() {
    assert!(!accepts_activitypub("text/html"));
    assert!(!accepts_activitypub("application/json"));
}

#[test]
fn rejects_empty_accept_header() {
    assert!(!accepts_activitypub(""));
}
