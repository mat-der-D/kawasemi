//! `nodeinfo` handlers (design.md "Endpoints（handlers）"; Requirements 5.1,
//! 5.2, 5.3; task 5.1, `Boundary: nodeinfo`): the NodeInfo discovery
//! document (`GET /.well-known/nodeinfo`) and the NodeInfo document itself
//! (`GET /nodeinfo/{version}`).
//!
//! Per design.md's Responsibilities for this handler: "ディスカバリリンク
//! （5.1）と最小統計ドキュメント（5.2）、内部情報は出さない（5.3）".
//!
//! ## Not wired into a router (task 5.4's job)
//! Mirrors `webfinger.rs`'s identical reasoning (itself mirroring
//! `src/oauth/apps_endpoint.rs`'s established precedent): these are plain
//! axum handlers shaped for `.route(...).with_state(...)`, exercised
//! directly by this module's own tests
//! (`src/federation/endpoints/nodeinfo/tests.rs`) and, for full
//! HTTP-observable behavior, by `tests/webfinger_nodeinfo_it.rs`'s
//! test-local `axum::Router`.
//!
//! ## Deliberately minimal document shape (design decision, Requirement
//! 5.2/5.3)
//! The real NodeInfo protocol (<https://nodeinfo.diaspora.software/>)
//! defines a larger required envelope for its document (`usage`,
//! `openRegistrations`, `services`, `metadata`, ...) than this spec's own
//! Requirement 5.2 asks for. Requirement 5.2 only requires "ソフトウェア
//! 名・バージョン・対応プロトコル（ActivityPub）" (software name, version,
//! supported protocols), and Requirement 5.3 explicitly forbids anything
//! beyond public, spec'd information ("外部公開を意図しない内部情報を含め
//! ない"). Rather than fabricating placeholder values for the protocol's
//! other required fields (e.g. a hardcoded `"usage": {"users": {"total": 0}}`
//! that misrepresents real instance state, or a real DB-derived count that
//! this task's own "no internal counts/config beyond what's spec'd" scope
//! explicitly excludes), [`NodeInfoDocument`] emits exactly the three
//! fields Requirement 5.2 names plus the schema `version` string every
//! NodeInfo consumer needs to interpret the document at all. Widening this
//! to full upstream-NodeInfo-schema compliance (real `usage` statistics,
//! `openRegistrations`, etc.) is left to a future task/spec that actually
//! owns computing those numbers.
//!
//! ## Discovery link target: only NodeInfo 2.0 is offered
//! [`NODEINFO_SCHEMA_VERSION`]/[`NODEINFO_SCHEMA_NAMESPACE`] pin this
//! instance to NodeInfo schema 2.0 (<http://nodeinfo.diaspora.software/ns/schema/2.0>,
//! the most widely interoperable version among existing ActivityPub
//! implementations); [`nodeinfo_document`] rejects every other `{version}`
//! path segment with `404` (design.md's API Contract for
//! `GET /nodeinfo/{ver}`). Not a requirements-driven choice -- design.md
//! does not name a specific version -- but a concrete one this task must
//! make to have anything to serve at all.

#[cfg(test)]
mod tests;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use serde::Serialize;

use crate::error::AppError;

/// This instance's reported software name (Requirement 5.2). Matches
/// `Cargo.toml`'s package name.
const SOFTWARE_NAME: &str = "kawasemi";

/// The NodeInfo schema version this instance serves (Requirement 5.2). See
/// this module's doc comment ("Discovery link target").
const NODEINFO_SCHEMA_VERSION: &str = "2.0";

/// The NodeInfo discovery `rel` value naming the schema 2.0 namespace
/// (upstream NodeInfo protocol convention), used as this instance's sole
/// discovery link (Requirement 5.1).
const NODEINFO_SCHEMA_NAMESPACE: &str = "http://nodeinfo.diaspora.software/ns/schema/2.0";

/// The single protocol this instance reports support for (Requirement
/// 5.2's "ActivityPub").
const ACTIVITYPUB_PROTOCOL: &str = "activitypub";

/// Everything the NodeInfo handlers need, bundled behind one
/// `axum::extract::State`-compatible handle. Only [`nodeinfo_discovery`]
/// actually reads `domain` (to build the discovery link's absolute `href`);
/// [`nodeinfo_document`] takes it too for uniformity of `.with_state(...)`
/// wiring (mirrors `WebfingerState`'s shape), even though it does not
/// currently use it.
#[derive(Debug, Clone)]
pub struct NodeInfoState {
    /// This instance's own configured domain (`ServerConfig::domain`), used
    /// to build the discovery document's absolute link `href`.
    pub domain: String,
}

/// One `.well-known/nodeinfo` discovery link (Requirement 5.1).
#[derive(Debug, Clone, Serialize)]
struct NodeInfoDiscoveryLink {
    rel: String,
    href: String,
}

/// The `.well-known/nodeinfo` discovery document body (Requirement 5.1).
#[derive(Debug, Clone, Serialize)]
struct NodeInfoDiscovery {
    links: Vec<NodeInfoDiscoveryLink>,
}

/// The `software` block of a NodeInfo document (Requirement 5.2).
#[derive(Debug, Clone, Serialize)]
struct NodeInfoSoftware {
    name: String,
    version: String,
}

/// A NodeInfo document (Requirement 5.2). See this module's doc comment
/// ("Deliberately minimal document shape") for why this does not carry the
/// full upstream NodeInfo schema.
#[derive(Debug, Clone, Serialize)]
struct NodeInfoDocument {
    version: String,
    software: NodeInfoSoftware,
    protocols: Vec<String>,
}

/// `GET /.well-known/nodeinfo` (Requirement 5.1): returns the set of links
/// to this instance's available NodeInfo document location(s) -- exactly
/// one, the schema-2.0 document at `/nodeinfo/2.0`.
pub async fn nodeinfo_discovery(State(state): State<NodeInfoState>) -> Json<serde_json::Value> {
    let discovery = NodeInfoDiscovery {
        links: vec![NodeInfoDiscoveryLink {
            rel: NODEINFO_SCHEMA_NAMESPACE.to_string(),
            href: format!(
                "https://{}/nodeinfo/{}",
                state.domain, NODEINFO_SCHEMA_VERSION
            ),
        }],
    };
    Json(serde_json::to_value(discovery).expect("NodeInfoDiscovery always serializes to JSON"))
}

/// `GET /nodeinfo/{version}` (Requirement 5.2): returns the minimal public
/// NodeInfo document for `version`, or `404` for any version other than
/// [`NODEINFO_SCHEMA_VERSION`] (design.md's API Contract). Never includes
/// internal information (Requirement 5.3) -- see this module's doc comment
/// ("Deliberately minimal document shape").
pub async fn nodeinfo_document(
    State(_state): State<NodeInfoState>,
    Path(version): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    if version != NODEINFO_SCHEMA_VERSION {
        return Err(AppError::client(
            StatusCode::NOT_FOUND,
            format!("unsupported nodeinfo version {version:?}"),
        ));
    }

    let document = NodeInfoDocument {
        version: NODEINFO_SCHEMA_VERSION.to_string(),
        software: NodeInfoSoftware {
            name: SOFTWARE_NAME.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        protocols: vec![ACTIVITYPUB_PROTOCOL.to_string()],
    };

    Ok(Json(
        serde_json::to_value(document).expect("NodeInfoDocument always serializes to JSON"),
    ))
}
