//! Port specifications for node templates.
//!
//! A port is a typed input or output socket on a node. `PortSpec` describes
//! one port at the template level (every instance of the template has these
//! ports); a `Node` in a `Graph` then references them by name when wiring
//! `Edge`s.
//!
//! The `type_tag` is a free-form string for v1 (e.g. `"bytes"`, `"json"`,
//! `"http.request"`). Sections 4 and 9 will tighten this into a coherent
//! type system once the parser pack lands and we have concrete examples of
//! what semantic compatibility means.

use serde::{Deserialize, Serialize};

/// Cardinality of an edge attached to a port.
///
/// - `Single` — exactly one edge required.
/// - `Optional` — at most one edge; absence is legal.
/// - `Many` — any number of edges (used by fan-out outputs and merge inputs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortMultiplicity {
    Single,
    Optional,
    Many,
}

/// One typed port on a node template. Cloneable so iteration over a
/// template's port list doesn't borrow the trait object.
///
/// The fields are wire-shaped (snake_case serde) because the same struct is
/// surfaced over `GET /api/templates/:id` to the frontend node palette.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortSpec {
    /// Unique-within-template name. Referenced by `Edge.source_port` /
    /// `Edge.target_port`.
    pub name: String,
    /// Semantic type tag — opaque string for v1. See module docs.
    pub type_tag: String,
    pub multiplicity: PortMultiplicity,
    /// One-line human description; surfaced in the UI's port tooltip.
    pub doc: String,
}

impl PortSpec {
    /// Convenience constructor for the common case of a single-arity port
    /// with a doc string. Keeps built-in template definitions terse.
    pub fn single(name: &str, type_tag: &str, doc: &str) -> Self {
        Self {
            name: name.to_string(),
            type_tag: type_tag.to_string(),
            multiplicity: PortMultiplicity::Single,
            doc: doc.to_string(),
        }
    }
}
