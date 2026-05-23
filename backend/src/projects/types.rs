//! Persisted domain model for projects + graphs.
//!
//! Two schema-versioned documents live on disk under `projects/<slug>/`:
//! - `project.json` → `Project` / `ProjectMeta` (identity + timestamps)
//! - `graph.json`   → `Graph` (nodes + edges)
//!
//! ## The `Slug` security boundary
//!
//! `Slug` is the *only* type permitted to name a project on disk. Every code
//! path that joins a slug into a filesystem path goes through this type. The
//! invariants enforced by [`Slug::new`] are stricter than they need to be for
//! pure URL-safety because the same value is reused as a *path segment* — so
//! the validator rejects `..`, `.`, `/`, `\`, NUL, control characters, and
//! Windows-reserved device names regardless of the host OS. Defense in depth
//! is cheaper than another CVE.
//!
//! The custom `Deserialize` impl routes JSON deserialisation through
//! [`Slug::new`], so an attacker cannot smuggle a malformed slug through a
//! request body even if the handler accepts `Slug` directly.

use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;
use time::OffsetDateTime;

/// Current schema version of the on-disk `project.json` document.
pub const PROJECT_SCHEMA_VERSION: u32 = 1;

/// Current schema version of the on-disk `graph.json` document.
pub const GRAPH_SCHEMA_VERSION: u32 = 1;

const SLUG_MIN_LEN: usize = 2;
const SLUG_MAX_LEN: usize = 40;

// Windows reserved device names. Even on Unix we reject these as project
// slugs so a project folder copied to a Windows host (e.g. via git on a
// shared filesystem) does not fail to open. Cheap, defense in depth.
const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul",
    "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9",
    "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Reasons a candidate string was rejected as a slug. The variant is logged
/// server-side; the client surfaces a single sanitised message via `ApiError`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SlugError {
    #[error("slug must be {SLUG_MIN_LEN}-{SLUG_MAX_LEN} characters; got {0}")]
    Length(usize),
    #[error("slug must start with a lowercase letter")]
    BadStart,
    #[error("slug must end with a lowercase letter or digit")]
    BadEnd,
    #[error("slug character at byte {0} is not in [a-z0-9-]")]
    BadChar(usize),
    #[error("slug is a reserved device name")]
    Reserved,
}

/// A validated project identifier. Cheap to clone (single `String` field).
///
/// Construct via [`Slug::new`] or via `parse()`/`FromStr` / `TryFrom<&str>`.
/// `Deserialize` routes through the validator so JSON-borne slugs cannot
/// bypass the rules.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Slug(String);

impl Slug {
    /// Validate `raw` against the slug ruleset and return an owned `Slug`.
    ///
    /// Rules (enforced in order — earlier failures shadow later ones):
    /// 1. Length is between [`SLUG_MIN_LEN`] and [`SLUG_MAX_LEN`].
    /// 2. First byte is `[a-z]`. Digit-first and hyphen-first are rejected
    ///    so slugs sort sensibly and never collide with numeric IDs.
    /// 3. Last byte is `[a-z0-9]`. Trailing hyphen is rejected.
    /// 4. Every interior byte is one of `[a-z0-9-]`. Anything else — including
    ///    `..`, `/`, `\`, NUL, control characters, uppercase, underscore,
    ///    multibyte UTF-8 — is rejected at the byte that violates the rule.
    /// 5. The lowercased candidate is not in [`RESERVED_NAMES`].
    pub fn new(raw: &str) -> Result<Self, SlugError> {
        let bytes = raw.as_bytes();
        let len = bytes.len();
        if !(SLUG_MIN_LEN..=SLUG_MAX_LEN).contains(&len) {
            return Err(SlugError::Length(len));
        }

        let first = bytes[0];
        if !first.is_ascii_lowercase() {
            return Err(SlugError::BadStart);
        }

        let last = bytes[len - 1];
        if !(last.is_ascii_lowercase() || last.is_ascii_digit()) {
            return Err(SlugError::BadEnd);
        }

        for (i, &b) in bytes.iter().enumerate() {
            let allowed = b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-';
            if !allowed {
                return Err(SlugError::BadChar(i));
            }
        }

        if RESERVED_NAMES.contains(&raw) {
            return Err(SlugError::Reserved);
        }

        Ok(Slug(raw.to_owned()))
    }

    /// Borrow the underlying string. Safe to use as a filesystem path
    /// component — every byte has been validated.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Slug {
    type Err = SlugError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Slug::new(s)
    }
}

impl TryFrom<&str> for Slug {
    type Error = SlugError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Slug::new(value)
    }
}

impl TryFrom<String> for Slug {
    type Error = SlugError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Slug::new(&value)
    }
}

// Hand-rolled `Deserialize` so a malformed slug arriving in a JSON request
// body fails closed at deserialisation time — handlers never see invalid
// `Slug` values. Without this, `#[derive(Deserialize)]` would skip the
// validator entirely.
impl<'de> Deserialize<'de> for Slug {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Slug::new(&raw).map_err(serde::de::Error::custom)
    }
}

/// Lightweight metadata header returned by `GET /api/projects`. The full
/// `Project` document also includes this struct as `meta`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub slug: Slug,
    pub name: String,
    /// Timestamps are serialised in RFC 3339 / ISO 8601 (the default for
    /// `time::OffsetDateTime` under the `serde-well-known` feature).
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub schema_version: u32,
}

/// Full project document persisted at `projects/<slug>/project.json`.
///
/// The graph is intentionally NOT inlined here — it has its own file
/// (`graph.json`) so the studio can edit the metadata and the graph
/// independently and so a large graph doesn't bloat list responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    #[serde(flatten)]
    pub meta: ProjectMeta,
}

/// Stable identifier for a graph node. The studio assigns UUIDs at create
/// time; the frontend treats it as opaque.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub String);

/// Stable identifier for a graph edge.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EdgeId(pub String);

/// Visual position on the ReactFlow canvas. The studio persists position so
/// the layout survives reloads.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

/// Legacy closed enumeration of node kinds from Section 2.
///
/// **Replaced in Section 3** by the open template registry. The variants
/// remain only as a stable mapping target so existing on-disk graphs that
/// shipped `kind: "route"` continue to load. New graphs and all in-memory
/// state use `Node.template_id: TemplateId`. The mapping function
/// [`legacy_kind_to_template_id`] is the only consumer of these variants
/// in code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Route,
    Handler,
    Service,
    Dto,
    Consumer,
    Scheduler,
    Logger,
}

impl NodeKind {
    /// Canonical S3+ template id for this legacy kind. Frozen mapping —
    /// changing it would break every persisted v1 graph on load.
    pub fn to_template_id(&self) -> crate::templates::TemplateId {
        let raw = match self {
            NodeKind::Route => "http.route",
            NodeKind::Handler => "http.handler",
            NodeKind::Service => "core.service",
            NodeKind::Dto => "core.dto",
            NodeKind::Consumer => "integration.consumer.placeholder",
            NodeKind::Scheduler => "integration.scheduler.placeholder",
            NodeKind::Logger => "observability.logger",
        };
        crate::templates::TemplateId::new(raw)
            .expect("legacy NodeKind mappings are validated at compile time")
    }
}

/// One node in the user's flow graph.
///
/// ## Wire shape
///
/// S3+ canonical: `{"id": ..., "template_id": "http.route", "position": ..., "config": ..., "label": ...}`.
///
/// Section 2 legacy: `{"id": ..., "kind": "route", "position": ..., ...}`.
/// The custom `Deserialize` impl accepts both shapes; the legacy form is
/// translated to the canonical template id via [`NodeKind::to_template_id`]
/// and logged at WARN level so operators see the migration happen. On
/// serialise, only the canonical shape is emitted — the next `save_graph`
/// rewrites the on-disk JSON, so legacy graphs migrate themselves the
/// first time a user touches them.
#[derive(Debug, Clone, Serialize)]
pub struct Node {
    pub id: NodeId,
    pub template_id: crate::templates::TemplateId,
    pub position: Position,
    /// Untyped config bag at this layer — `TemplateRegistry::validate` checks
    /// it against the template's JSON Schema at `save_graph` time
    /// (`backend/src/projects/store.rs`).
    #[serde(default)]
    pub config: serde_json::Value,
    /// Optional user-friendly label; UI falls back to `id` / template name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Optional developer note/comment for this node in the visual builder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

// Backward-compatible Deserialize — accepts both the S2 legacy shape (with
// `kind`) and the S3 canonical shape (with `template_id`). If both are
// present, `template_id` wins.
impl<'de> Deserialize<'de> for Node {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Stage one: collect the JSON Value, then route through either of
        // the two intermediate shapes. This is slightly less efficient than
        // a Visitor but dramatically easier to reason about — and
        // graph-deserialise is not a hot path.
        let raw = serde_json::Value::deserialize(deserializer)?;
        let obj = raw
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("Node must be a JSON object"))?;

        let id: NodeId = obj
            .get("id")
            .ok_or_else(|| serde::de::Error::missing_field("id"))
            .and_then(|v| serde_json::from_value(v.clone()).map_err(serde::de::Error::custom))?;
        let position: Position = obj
            .get("position")
            .ok_or_else(|| serde::de::Error::missing_field("position"))
            .and_then(|v| serde_json::from_value(v.clone()).map_err(serde::de::Error::custom))?;
        let config: serde_json::Value = obj
            .get("config")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let label: Option<String> = obj
            .get("label")
            .and_then(|v| v.as_str().map(|s| s.to_string()));
        let comment: Option<String> = obj
            .get("comment")
            .and_then(|v| v.as_str().map(|s| s.to_string()));

        // Resolve template_id with backward compatibility:
        //   - If `template_id` is present, deserialise through TemplateId's
        //     hand-rolled validator (security boundary parity).
        //   - Else if `kind` is present, map via NodeKind and warn.
        //   - Else error with a clear message naming both accepted fields.
        let template_id = if let Some(v) = obj.get("template_id") {
            serde_json::from_value::<crate::templates::TemplateId>(v.clone())
                .map_err(serde::de::Error::custom)?
        } else if let Some(v) = obj.get("kind") {
            let kind: NodeKind =
                serde_json::from_value(v.clone()).map_err(serde::de::Error::custom)?;
            let template_id = kind.to_template_id();
            tracing::warn!(
                node_id = %id.0,
                legacy_kind = ?kind,
                template_id = %template_id,
                "node uses S2 legacy `kind` field; will be rewritten as `template_id` on next save"
            );
            template_id
        } else {
            return Err(serde::de::Error::custom(
                "Node must carry either `template_id` (S3+) or `kind` (S2 legacy)",
            ));
        };

        Ok(Node {
            id,
            template_id,
            position,
            config,
            label,
            comment,
        })
    }
}

/// One directed edge between two nodes' named ports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub id: EdgeId,
    pub source: NodeId,
    pub target: NodeId,
    pub source_port: String,
    pub target_port: String,
}

/// The flow graph persisted at `projects/<slug>/graph.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph {
    pub schema_version: u32,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

impl Default for Graph {
    /// Empty graph at the current schema version. Used when a brand-new
    /// project is created.
    fn default() -> Self {
        Self {
            schema_version: GRAPH_SCHEMA_VERSION,
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }
}

impl Graph {
    /// Return every node that has an outgoing edge into `node_id`'s
    /// `target_port`.  Order follows edge order in `self.edges`.
    ///
    /// Used by templates at codegen time to discover upstream dependencies
    /// (e.g. a handler finding the services wired to its `request` port).
    pub fn upstream_of<'a>(&'a self, node_id: &NodeId, target_port: &str) -> Vec<&'a Node> {
        self.edges
            .iter()
            .filter(|e| e.target == *node_id && e.target_port == target_port)
            .filter_map(|e| self.nodes.iter().find(|n| n.id == e.source))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Centralised happy-path table; every accepted slug here is also used
    /// implicitly in `test_slug_round_trips_through_json`.
    fn good_slugs() -> &'static [&'static str] {
        &[
            "ab",
            "ax",
            "abc",
            "user-service",
            "my-cool-project-42",
            "a1",
            "rust2024",
            "p-q-r-s",
            "a-very-long-slug-that-is-exactly-40-cha",
        ]
    }

    /// Each entry maps an adversarial input to the variant we expect it to
    /// be rejected with. Lock these in tests so a future "cleanup" can't
    /// silently widen the validator.
    fn bad_slugs() -> &'static [(&'static str, fn(&SlugError) -> bool)] {
        &[
            ("", |e| matches!(e, SlugError::Length(0))),
            ("a", |e| matches!(e, SlugError::Length(1))),
            // 41-char slug: starts with 'a' but is 1 over the cap.
            (
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                |e| matches!(e, SlugError::Length(41)),
            ),
            ("Abc", |e| matches!(e, SlugError::BadStart)),
            ("-abc", |e| matches!(e, SlugError::BadStart)),
            ("0abc", |e| matches!(e, SlugError::BadStart)),
            ("_abc", |e| matches!(e, SlugError::BadStart)),
            ("abc-", |e| matches!(e, SlugError::BadEnd)),
            // Trailing illegal chars hit `BadEnd` before `BadChar` — rule
            // ordering documented on `Slug::new`. Underscore + NUL trail are
            // both rejected here; interior cases below land in `BadChar`.
            ("abc_", |e| matches!(e, SlugError::BadEnd)),
            ("ab.c", |e| matches!(e, SlugError::BadChar(_))),
            ("a/b", |e| matches!(e, SlugError::BadChar(_))),
            ("a\\b", |e| matches!(e, SlugError::BadChar(_))),
            ("..", |e| matches!(e, SlugError::BadStart)),
            ("ab\0", |e| matches!(e, SlugError::BadEnd)),
            ("héllo", |e| matches!(e, SlugError::BadChar(_))),
            ("con", |e| matches!(e, SlugError::Reserved)),
            ("nul", |e| matches!(e, SlugError::Reserved)),
            ("com1", |e| matches!(e, SlugError::Reserved)),
        ]
    }

    #[test]
    fn test_slug_accepts_all_documented_happy_paths() {
        for s in good_slugs() {
            assert!(Slug::new(s).is_ok(), "expected {s:?} to be accepted");
        }
    }

    #[test]
    fn test_slug_rejects_all_adversarial_inputs() {
        for (raw, matcher) in bad_slugs() {
            let err = Slug::new(raw).unwrap_err();
            assert!(
                matcher(&err),
                "wrong error variant for {raw:?}: got {err:?}"
            );
        }
    }

    #[test]
    fn test_slug_round_trips_through_json() {
        let s = Slug::new("user-service").unwrap();
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"user-service\"");
        let back: Slug = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn test_slug_deserialize_rejects_invalid_json_payloads() {
        for (raw, _) in bad_slugs() {
            let json = serde_json::to_string(raw).unwrap();
            let result: Result<Slug, _> = serde_json::from_str(&json);
            assert!(
                result.is_err(),
                "Slug deserialize should have rejected {raw:?}"
            );
        }
    }

    #[test]
    fn test_graph_default_is_empty_at_current_schema_version() {
        let g = Graph::default();
        assert_eq!(g.schema_version, GRAPH_SCHEMA_VERSION);
        assert!(g.nodes.is_empty());
        assert!(g.edges.is_empty());
    }

    #[test]
    fn test_node_kind_serialises_snake_case() {
        let s = serde_json::to_string(&NodeKind::Dto).unwrap();
        assert_eq!(s, "\"dto\"");
        let back: NodeKind = serde_json::from_str("\"scheduler\"").unwrap();
        assert_eq!(back, NodeKind::Scheduler);
    }

    #[test]
    fn test_node_kind_rejects_unknown_kind() {
        let r: Result<NodeKind, _> = serde_json::from_str("\"frobulator\"");
        assert!(r.is_err(), "unknown kinds must not deserialise");
    }

    #[test]
    fn test_legacy_kind_maps_to_canonical_template_id() {
        // Frozen mapping — any change breaks persisted v1 graphs.
        let cases = [
            (NodeKind::Route, "http.route"),
            (NodeKind::Handler, "http.handler"),
            (NodeKind::Service, "core.service"),
            (NodeKind::Dto, "core.dto"),
            (NodeKind::Consumer, "integration.consumer.placeholder"),
            (NodeKind::Scheduler, "integration.scheduler.placeholder"),
            (NodeKind::Logger, "observability.logger"),
        ];
        for (kind, expected) in cases {
            assert_eq!(kind.to_template_id().as_str(), expected);
        }
    }

    #[test]
    fn test_node_deserialises_s3_canonical_shape() {
        let node: Node = serde_json::from_value(serde_json::json!({
            "id": "n1",
            "template_id": "http.route",
            "position": {"x": 10.0, "y": 20.0},
            "config": {"path": "/", "method": "GET"},
        }))
        .unwrap();
        assert_eq!(node.id.0, "n1");
        assert_eq!(node.template_id.as_str(), "http.route");
    }

    #[test]
    fn test_node_deserialises_s2_legacy_kind_field() {
        // S2 graphs ship `kind: "route"`; must still load post-S3.
        let node: Node = serde_json::from_value(serde_json::json!({
            "id": "old1",
            "kind": "route",
            "position": {"x": 0.0, "y": 0.0},
            "config": {"path": "/legacy", "method": "GET"},
        }))
        .unwrap();
        assert_eq!(node.id.0, "old1");
        assert_eq!(node.template_id.as_str(), "http.route");
    }

    #[test]
    fn test_node_with_both_kind_and_template_id_prefers_template_id() {
        let node: Node = serde_json::from_value(serde_json::json!({
            "id": "n",
            "kind": "route",
            "template_id": "core.service",
            "position": {"x": 0.0, "y": 0.0},
        }))
        .unwrap();
        assert_eq!(node.template_id.as_str(), "core.service");
    }

    #[test]
    fn test_node_with_neither_kind_nor_template_id_errors() {
        let r: Result<Node, _> = serde_json::from_value(serde_json::json!({
            "id": "n",
            "position": {"x": 0.0, "y": 0.0},
        }));
        assert!(r.is_err());
    }

    #[test]
    fn test_node_serialises_only_canonical_shape() {
        // Load legacy → serialise → no `kind` field; only `template_id`.
        let node: Node = serde_json::from_value(serde_json::json!({
            "id": "n",
            "kind": "logger",
            "position": {"x": 0.0, "y": 0.0},
        }))
        .unwrap();
        let out = serde_json::to_value(&node).unwrap();
        let obj = out.as_object().unwrap();
        assert!(obj.contains_key("template_id"));
        assert!(!obj.contains_key("kind"), "serialise must not emit legacy field");
        assert_eq!(obj["template_id"], "observability.logger");
    }

    #[test]
    fn test_node_rejects_unknown_legacy_kind() {
        let r: Result<Node, _> = serde_json::from_value(serde_json::json!({
            "id": "n",
            "kind": "frobulator",
            "position": {"x": 0.0, "y": 0.0},
        }));
        assert!(r.is_err(), "unknown legacy kind should still be rejected");
    }
}
