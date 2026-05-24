//! Edge-type resolver — Section 1 of the post-CEP roadmap (6.7 in `ROADMAP_CEP.md`).
//!
//! The studio's port system declares a static `type_tag: String` on every
//! port at template-construction time (see `templates::ports::PortSpec`).
//! That tag is fine for templates whose emitted Rust type never changes
//! (`http.route` always emits a `Request<Bytes>`), but it cannot express the
//! type of a `language.struct` node *named* `User`, a `custom.block` whose
//! signature is parsed at save time, or a function node whose return type is
//! driven by config. This module introduces a separate **resolved-type**
//! layer that sits *over* the static tag: every `(NodeId, port_name, PortSide)`
//! gets a `ResolvedType`, derived by asking the owning template (T2/T3) and
//! falling back to the static tag when the template has not opted in.
//!
//! The resolver is read-only and built once per graph at save / validate
//! time. It does not allocate locks or atomics — it is a plain map produced
//! by `for_graph(...)` and consumed by validation, codegen, and the wire
//! layer. Downstream sections of the roadmap (operators, test runner,
//! connector pack) will read from it; this section just constructs and
//! exposes it.

use std::collections::HashMap;

use crate::projects::types::{Graph, NodeId};
use crate::templates::TemplateRegistry;

/// Which side of an edge a port participates on.
///
/// The same port name can exist on both the input and output side of a node
/// (rare but legal), so the resolver keys on `(NodeId, name, PortSide)`
/// rather than `(NodeId, name)` alone. Codegen and validation use the side
/// to disambiguate when querying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortSide {
    /// Output port — value flows *out of* the node along this port.
    Source,
    /// Input port — value flows *into* the node along this port.
    Target,
}

/// A concrete Rust type expression that flows across an edge.
///
/// Stored as a `String` rather than a `syn::Type` because the value is also
/// the wire shape returned to the frontend (badges on edges) and to other
/// services (LLM context); a `String` round-trips through JSON without
/// gymnastics. Equality / compatibility is performed structurally by parsing
/// to `syn::Type` and comparing token streams (see T4); plain `String`
/// equality is *not* a sufficient compat check because `Vec<u8>` and
/// `Vec < u8 >` would falsely diverge.
///
/// The value held here is always the *Rust* type, not the legacy free-form
/// `type_tag` (`"http.request"`, `"any"`). Fallback retains the tag verbatim
/// — downstream consumers must inspect [`ResolvedType::is_legacy_tag`] when
/// they need to distinguish a real Rust type from an opaque tag inherited
/// from a non-opted-in template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedType {
    raw: String,
    legacy: bool,
}

impl ResolvedType {
    /// Construct from a Rust type expression published by an opted-in
    /// template. Marks the value as a real type, not a legacy tag.
    pub fn rust(ty: impl Into<String>) -> Self {
        Self {
            raw: ty.into(),
            legacy: false,
        }
    }

    /// Construct from the static `type_tag` of a non-opted-in template.
    /// Validation treats these with the old wildcard/exact-match rules; new
    /// type-driven validation only fires when both sides are non-legacy.
    pub fn legacy_tag(tag: impl Into<String>) -> Self {
        Self {
            raw: tag.into(),
            legacy: true,
        }
    }

    /// Raw type string. For a Rust-typed port this is something like
    /// `"i32"` or `"crate::types::user::User"`; for a legacy tag this is the
    /// opaque tag verbatim (`"http.request"`, `"any"`).
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// `true` when this value originated from a static `type_tag` fallback.
    /// Validation uses this to decide which comparison rule to apply.
    pub fn is_legacy_tag(&self) -> bool {
        self.legacy
    }
}

/// Per-edge port-type cache for one graph.
///
/// Built once via [`TypeResolver::for_graph`] and queried by validation
/// (T4) and codegen (T5). Insertion order is deterministic but unspecified
/// — callers must key by `(NodeId, port, side)` rather than iteration order.
#[derive(Debug, Default)]
pub struct TypeResolver {
    by_port: HashMap<(NodeId, String, PortSide), ResolvedType>,
}

impl TypeResolver {
    /// Empty resolver. Useful for tests that construct ports manually.
    pub fn new() -> Self {
        Self::default()
    }

    /// Compute resolved types for every port mentioned by an edge in
    /// `graph`. Ports never touched by an edge are not inserted — the
    /// resolver is a sparse view of "what types are actually flowing".
    ///
    /// Algorithm:
    /// 1. Walk `graph.edges`; for each end (source/target), look up the
    ///    node, then the node's template, then the port on that template.
    /// 2. If the template opts in via `resolve_port_type` (T2), use that.
    ///    Otherwise fall back to the static `PortSpec.type_tag`.
    /// 3. Ignore edges referencing missing nodes or missing templates —
    ///    validation (which runs *before* the resolver is consumed) is the
    ///    place to surface those; the resolver must remain panic-free.
    ///
    /// O(E) in the number of edges; each port lookup is O(P) in the
    /// template's port list (templates carry ≤ ~10 ports, so this is fine).
    pub fn for_graph(graph: &Graph, registry: &TemplateRegistry) -> Self {
        let mut by_port = HashMap::new();

        // Node lookup for O(1) template-id resolution.
        let nodes: HashMap<&NodeId, &crate::projects::types::Node> =
            graph.nodes.iter().map(|n| (&n.id, n)).collect();

        for edge in &graph.edges {
            // Source side (output port on source node).
            if let Some(node) = nodes.get(&edge.source) {
                if let Some(t) = registry.get(&node.template_id) {
                    if let Some(spec) = t
                        .output_ports()
                        .iter()
                        .find(|p| p.name == edge.source_port)
                    {
                        let resolved = ResolvedType::legacy_tag(&spec.type_tag);
                        by_port.insert(
                            (edge.source.clone(), edge.source_port.clone(), PortSide::Source),
                            resolved,
                        );
                    }
                }
            }
            // Target side (input port on target node).
            if let Some(node) = nodes.get(&edge.target) {
                if let Some(t) = registry.get(&node.template_id) {
                    if let Some(spec) = t
                        .input_ports()
                        .iter()
                        .find(|p| p.name == edge.target_port)
                    {
                        let resolved = ResolvedType::legacy_tag(&spec.type_tag);
                        by_port.insert(
                            (edge.target.clone(), edge.target_port.clone(), PortSide::Target),
                            resolved,
                        );
                    }
                }
            }
        }

        Self { by_port }
    }

    /// Look up the resolved type for one `(node, port, side)`. Returns
    /// `None` when the port was not referenced by any edge or the
    /// underlying node/template/port did not exist.
    pub fn get(&self, node: &NodeId, port: &str, side: PortSide) -> Option<&ResolvedType> {
        self.by_port.get(&(node.clone(), port.to_string(), side))
    }

    /// Number of `(node, port, side)` entries cached. Test introspection
    /// helper; production callers should use `get` directly.
    pub fn len(&self) -> usize {
        self.by_port.len()
    }

    /// `true` when the resolver holds no entries — every edge in the graph
    /// either had no resolvable port or the graph had no edges at all.
    pub fn is_empty(&self) -> bool {
        self.by_port.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::types::{
        Edge, EdgeId, Graph, Node, NodeId, Position, GRAPH_SCHEMA_VERSION,
    };
    use crate::templates::TemplateId;
    use serde_json::Value;

    fn node(id: &str, template_id: &str) -> Node {
        Node {
            id: NodeId(id.into()),
            template_id: TemplateId::new(template_id).unwrap(),
            position: Position { x: 0.0, y: 0.0 },
            config: Value::Null,
            label: None,
            comment: None,
        }
    }

    fn edge(id: &str, source: &str, source_port: &str, target: &str, target_port: &str) -> Edge {
        Edge {
            id: EdgeId(id.into()),
            source: NodeId(source.into()),
            target: NodeId(target.into()),
            source_port: source_port.into(),
            target_port: target_port.into(),
        }
    }

    fn registry() -> TemplateRegistry {
        TemplateRegistry::with_builtins()
    }

    /// Validates the fallback path: a template that hasn't opted in (T2/T3
    /// add the opt-in hook) yields a `ResolvedType` whose `as_str()`
    /// matches the template's static `type_tag` and whose `is_legacy_tag()`
    /// is `true`.
    #[test]
    fn test_resolver_falls_back_to_static_tag() {
        let g = Graph {
            schema_version: GRAPH_SCHEMA_VERSION,
            nodes: vec![node("n1", "http.route"), node("n2", "http.handler")],
            edges: vec![edge("e1", "n1", "request", "n2", "request")],
        };
        let r = TypeResolver::for_graph(&g, &registry());

        let source = r
            .get(&NodeId("n1".into()), "request", PortSide::Source)
            .expect("source port must resolve");
        assert!(source.is_legacy_tag());
        assert!(!source.as_str().is_empty());

        let target = r
            .get(&NodeId("n2".into()), "request", PortSide::Target)
            .expect("target port must resolve");
        assert!(target.is_legacy_tag());
        assert!(!target.as_str().is_empty());
    }

    /// Calling `for_graph` twice on the same input yields a resolver whose
    /// `len()` matches — guards against accidental double-insertion when
    /// the same edge surfaces both endpoints.
    #[test]
    fn test_resolver_caches_per_port() {
        let g = Graph {
            schema_version: GRAPH_SCHEMA_VERSION,
            nodes: vec![node("n1", "http.route"), node("n2", "http.handler")],
            edges: vec![edge("e1", "n1", "request", "n2", "request")],
        };
        let r1 = TypeResolver::for_graph(&g, &registry());
        let r2 = TypeResolver::for_graph(&g, &registry());
        assert_eq!(r1.len(), r2.len());
        // One source-side entry plus one target-side entry for the one edge.
        assert_eq!(r1.len(), 2);
    }

    /// Dangling edges (source / target node not in `graph.nodes`) must not
    /// panic and must not produce stray entries — the resolver is a
    /// best-effort view; validation surfaces dangling errors separately.
    #[test]
    fn test_resolver_handles_dangling_node() {
        let g = Graph {
            schema_version: GRAPH_SCHEMA_VERSION,
            nodes: vec![node("n1", "http.route")],
            edges: vec![edge("e1", "n1", "request", "ghost", "request")],
        };
        let r = TypeResolver::for_graph(&g, &registry());

        // Source side of the (real) node resolves.
        assert!(r
            .get(&NodeId("n1".into()), "request", PortSide::Source)
            .is_some());
        // Target side (dangling node) is silently skipped.
        assert!(r
            .get(&NodeId("ghost".into()), "request", PortSide::Target)
            .is_none());
    }

    /// Empty graph yields an empty resolver — `is_empty()` must agree with
    /// the `len() == 0` view.
    #[test]
    fn test_resolver_empty_graph_is_empty() {
        let g = Graph::default();
        let r = TypeResolver::for_graph(&g, &registry());
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    /// Edge whose source port name does not exist on the source template
    /// (validation will catch this separately) must not panic and must not
    /// insert a stray entry.
    #[test]
    fn test_resolver_unknown_port_is_skipped() {
        let g = Graph {
            schema_version: GRAPH_SCHEMA_VERSION,
            nodes: vec![node("n1", "http.route"), node("n2", "http.handler")],
            edges: vec![edge("e1", "n1", "no_such_port", "n2", "request")],
        };
        let r = TypeResolver::for_graph(&g, &registry());

        assert!(r
            .get(&NodeId("n1".into()), "no_such_port", PortSide::Source)
            .is_none());
        // Target side still resolves — the half-broken edge still has a
        // valid target endpoint.
        assert!(r
            .get(&NodeId("n2".into()), "request", PortSide::Target)
            .is_some());
    }

    /// `ResolvedType::rust(...)` constructs a non-legacy type. Verifies the
    /// constructor labels the value correctly for T4's validation logic.
    #[test]
    fn test_resolved_type_rust_constructor_marks_non_legacy() {
        let t = ResolvedType::rust("i32");
        assert_eq!(t.as_str(), "i32");
        assert!(!t.is_legacy_tag());
    }

    /// `ResolvedType::legacy_tag(...)` constructs a legacy-tagged value.
    #[test]
    fn test_resolved_type_legacy_constructor_marks_legacy() {
        let t = ResolvedType::legacy_tag("http.request");
        assert_eq!(t.as_str(), "http.request");
        assert!(t.is_legacy_tag());
    }
}
