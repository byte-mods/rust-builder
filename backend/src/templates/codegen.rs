//! Codegen contract surface — types returned by [`crate::templates::NodeTemplate`]'s
//! `emit_runtime` and `emit_schema` methods.
//!
//! Section 4 implements the actual codegen pipeline (`syn` + `quote` +
//! `prettyplease`) and fills in the per-template `emit_*` bodies. Section 9's
//! parser pack uses both surfaces (runtime for byte-stream parsing,
//! codegen for schema-driven type generation). Section 13's step debugger
//! consumes the debug-bridge identifiers wired in alongside the runtime
//! emission.
//!
//! ## Why `String` for source rather than `syn::Item`
//!
//! Keeping the emitted source as a plain `String` for v1 avoids pulling the
//! `syn`/`quote` dependency into the template *contract* — only the codegen
//! *implementation* (S4) needs those crates. Templates author their output
//! via raw string literals or future helpers (`quote!` once S4 lands). The
//! source travels through `prettyplease::unparse` before hitting disk so
//! formatting is normalised.

use serde::Serialize;
use std::path::PathBuf;

/// Context handed to a template's `emit_*` methods.
///
/// Currently carries:
/// - the project's slug (so generated paths can reference the project name);
/// - the `Node` instance the emission is for (so config is inspectable);
/// - the project's output root (`projects/<slug>/`).
///
/// The graph and registry are *not* threaded in at v1 — templates emit
/// per-node, in isolation. S4 will introduce neighbour-aware ctx if a
/// concrete template needs it; until then keep the surface minimal.
pub struct CodegenCtx<'a> {
    pub project_slug: &'a str,
    pub node: &'a crate::projects::types::Node,
    pub output_root: &'a PathBuf,
    pub graph: &'a crate::projects::types::Graph,
}

/// One Rust source artefact produced by a template.
///
/// `module_path` is the relative path under `projects/<slug>/src/` where
/// the source should land — e.g. `handlers/hello.rs`, `dto/user.rs`. The
/// codegen orchestrator (S4) writes these out and merges multiple items
/// into the right module trees.
#[derive(Debug, Clone, Serialize)]
pub struct EmittedItem {
    pub module_path: String,
    pub source: String,
}

/// Result of a template's runtime emission — the Rust source that will run
/// at request/event time.
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeEmission {
    pub items: Vec<EmittedItem>,
    /// Cargo dependency hints — e.g. `("tokio", "1.36")`. The codegen
    /// orchestrator deduplicates these into the project's `Cargo.toml`.
    pub dependencies: Vec<(String, String)>,
    /// Identifier of the debug-bridge site that S4 will wrap this
    /// emission's entry point with. `None` means the template opts out of
    /// debug instrumentation (rare; the default trait impl always
    /// produces one).
    pub debug_site: Option<crate::templates::debug::DebugSiteId>,
}

impl RuntimeEmission {
    /// Placeholder emission used by the trait default and by built-ins
    /// whose codegen is deferred to a later section.
    pub fn not_implemented(template_id: &str) -> Self {
        Self {
            items: vec![EmittedItem {
                module_path: format!("generated/_pending_{}.rs", template_id.replace('.', "_")),
                source: format!(
                    "// Codegen for `{template_id}` lands in a later section.\n// See README.md roadmap.\n"
                ),
            }],
            dependencies: vec![],
            debug_site: None,
        }
    }

    /// Returns `true` if this emission is the `not_implemented` placeholder.
    /// The orchestrator uses this to skip writing pending files and to
    /// populate `GenerateReport.pending_templates`.
    pub fn is_placeholder(&self) -> bool {
        self.items.len() == 1
            && self.items[0]
                .module_path
                .starts_with("generated/_pending_")
    }
}

/// Result of a template's schema-time emission — Rust type definitions
/// derived from a schema file (e.g. `.proto`, `.fbs`, `.capnp`). Templates
/// in `CodegenMode::Runtime` typically return `not_implemented()` for this.
#[derive(Debug, Clone, Serialize)]
pub struct SchemaEmission {
    pub items: Vec<EmittedItem>,
    pub dependencies: Vec<(String, String)>,
}

impl SchemaEmission {
    /// Placeholder for runtime-only templates.
    pub fn not_implemented(_template_id: &str) -> Self {
        Self {
            items: vec![],
            dependencies: vec![],
        }
    }

    /// Returns `true` if this emission has no items (the schema default).
    pub fn is_placeholder(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_emission_not_implemented_carries_template_id_in_path() {
        let e = RuntimeEmission::not_implemented("parser.json");
        assert_eq!(e.items.len(), 1);
        assert!(e.items[0].module_path.contains("parser_json"));
        assert!(e.items[0].source.contains("parser.json"));
        assert!(e.dependencies.is_empty());
        assert!(e.debug_site.is_none());
    }

    #[test]
    fn test_schema_emission_not_implemented_is_empty() {
        let e = SchemaEmission::not_implemented("http.route");
        assert!(e.items.is_empty());
        assert!(e.dependencies.is_empty());
    }
}
