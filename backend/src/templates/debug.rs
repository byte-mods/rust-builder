//! Debug-bridge contract — types the step debugger (Section 13) reads to
//! know what to instrument.
//!
//! S3 ships only the identifiers and metadata. Section 13 wires the
//! WebSocket control protocol and the generated `--debug` mode harness on
//! the user-project side. Codegen (Section 4) consumes [`DebugSiteId`] to
//! produce the per-node wrapper calls.

use serde::{Deserialize, Serialize};

/// Stable, codegen-emitted identifier for one instrumentation site in a
/// generated user-project. Format: `<template_id>:<node_id>:<site_name>`,
/// e.g. `http.route:n1:before_handler`.
///
/// `DebugSiteId` is opaque to S3 — the debugger in S13 parses it at
/// runtime to decide what to show in the UI when the bridge halts there.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DebugSiteId(pub String);

impl DebugSiteId {
    pub fn new(template_id: &str, node_id: &str, site_name: &str) -> Self {
        Self(format!("{template_id}:{node_id}:{site_name}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Wire-shaped description of one debug site for the UI. Surfaced via the
/// project's `--debug` control socket (S13).
#[derive(Debug, Clone, Serialize)]
pub struct DebugSiteInfo {
    pub id: DebugSiteId,
    /// Template id this site belongs to — lets the UI render the right
    /// node-template icon when the bridge halts.
    pub template_id: String,
    /// Node id this site belongs to — used to highlight the node on the
    /// canvas.
    pub node_id: String,
    /// One-line description ("before handler call", "after parse", etc.)
    /// shown in the debugger panel.
    pub doc: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_debug_site_id_format() {
        let id = DebugSiteId::new("http.route", "n1", "before_handler");
        assert_eq!(id.as_str(), "http.route:n1:before_handler");
    }

    #[test]
    fn test_debug_site_id_round_trips_through_json() {
        let id = DebugSiteId::new("parser.json", "n42", "after_decode");
        let s = serde_json::to_string(&id).unwrap();
        assert_eq!(s, "\"parser.json:n42:after_decode\"");
        let back: DebugSiteId = serde_json::from_str(&s).unwrap();
        assert_eq!(id, back);
    }
}
