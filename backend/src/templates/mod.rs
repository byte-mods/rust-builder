//! Node template plugin contract — the architectural keystone of the studio.
//!
//! Sections 4 (codegen), 5 (long-runners), 7 (logger/error pass), 9 (parser
//! pack), 10 (system-engineering node pack), and 13 (visual step debugger)
//! all consume the [`NodeTemplate`] trait and the [`TemplateRegistry`].
//! Anything wrong with these surfaces echoes across four+ sections, so the
//! shapes here are deliberately minimal and additive-friendly: new
//! capabilities arrive as new trait methods with sensible defaults, not as
//! breaking changes.
//!
//! # Why the registry is immutable after construction
//!
//! Templates are compiled into the studio binary (locked v1 decision —
//! filesystem-loadable templates are a future extension). The registry is
//! built once in `main` via [`TemplateRegistry::with_builtins`] and
//! threaded into the router state behind an `Arc`. Read paths never
//! contend; there is no lock and no `RwLock` overhead. The only mutating
//! API is the builder used at startup.
//!
//! # Why `TemplateId` is its own type
//!
//! The wire format uses a namespaced opaque string (`"http.route"`,
//! `"parser.json"`). `TemplateId` validates that string at the boundary
//! the same way `Slug` does for project names — no untyped string ever
//! reaches the registry's `HashMap` key. The id has to match
//! `[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+`: lowercase, snake_case segments
//! separated by `.`, at least two segments. This avoids collisions with
//! kebab-case slugs (hyphens) and reserves a clean namespace boundary the
//! UI can split on for grouping.

pub mod codegen;
pub mod debug;
pub mod error;
pub mod handlers;
pub mod ports;

pub use handlers::templates_router;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use thiserror::Error;

pub use ports::{PortMultiplicity, PortSpec};

/// Codegen modes a template can declare. Used by Section 9's parser pack;
/// most templates are `Runtime` only.
///
/// - `Runtime`  — the template emits Rust source that runs at request/event
///   time. Standard for handlers, services, loggers.
/// - `Codegen`  — the template emits Rust type definitions at build time
///   from a schema file (e.g. `parser.protobuf` reading a `.proto`).
/// - `Both`     — the template offers both modes per instance, switchable
///   via the per-node config blob (the parser-pack default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodegenMode {
    Runtime,
    Codegen,
    Both,
}

/// Debug-bridge instrumentation contract — read by Section 13's step
/// debugger to know what kind of `await`/yield point to wrap each instance
/// with at codegen time. `Default` is suitable for any node whose work is a
/// single function call. Long-runners and async-stream nodes will override
/// in their templates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugBridgeKind {
    /// Plain `bridge.before(...); let r = work(...); bridge.after(...);`.
    Default,
    /// Long-running task that yields periodically (consumers, schedulers).
    LongRunner,
    /// Stream-shaped emit: bridge gets called per produced item.
    Stream,
    /// Pure pass-through (logger, router) — bridge only records traversal.
    PassThrough,
}

/// Stable identifier for a template kind. Namespaced opaque string with
/// validated character class. Cheap to clone.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct TemplateId(String);

/// Reasons a candidate string was rejected as a `TemplateId`. Variants are
/// logged server-side; the client sees a single sanitised message.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TemplateIdError {
    #[error("template id must contain at least one '.' (namespace.name)")]
    NoNamespace,
    #[error("template id must start with [a-z] (segment {0})")]
    BadStart(usize),
    #[error("template id segment {0} is empty")]
    EmptySegment(usize),
    #[error("template id char at byte {0} is not in [a-z0-9_.]")]
    BadChar(usize),
    #[error("template id length {0} exceeds maximum of 64")]
    TooLong(usize),
}

impl TemplateId {
    /// Maximum length cap — keeps registry keys bounded and prevents
    /// pathological inputs in the validation loop.
    pub const MAX_LEN: usize = 64;

    /// Validate and construct. Rules:
    /// 1. Length is between 1 and [`MAX_LEN`].
    /// 2. Every byte is one of `[a-z0-9_.]`.
    /// 3. Splitting on `.` yields at least 2 non-empty segments.
    /// 4. Every segment starts with `[a-z]`.
    pub fn new(raw: &str) -> Result<Self, TemplateIdError> {
        let len = raw.len();
        if len > Self::MAX_LEN {
            return Err(TemplateIdError::TooLong(len));
        }
        for (i, &b) in raw.as_bytes().iter().enumerate() {
            let ok = b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.';
            if !ok {
                return Err(TemplateIdError::BadChar(i));
            }
        }
        let segments: Vec<&str> = raw.split('.').collect();
        if segments.len() < 2 {
            return Err(TemplateIdError::NoNamespace);
        }
        for (i, seg) in segments.iter().enumerate() {
            if seg.is_empty() {
                return Err(TemplateIdError::EmptySegment(i));
            }
            let first = seg.as_bytes()[0];
            if !first.is_ascii_lowercase() {
                return Err(TemplateIdError::BadStart(i));
            }
        }
        Ok(TemplateId(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Namespace segment — everything before the first `.`. Stable by
    /// construction (the validator guarantees at least one `.`).
    pub fn namespace(&self) -> &str {
        self.0.split('.').next().unwrap_or("")
    }
}

impl fmt::Display for TemplateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// Hand-rolled `Deserialize` so a malformed id arriving via JSON fails closed
// at the wire boundary — same defense-in-depth pattern as `Slug`.
impl<'de> Deserialize<'de> for TemplateId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        TemplateId::new(&raw).map_err(serde::de::Error::custom)
    }
}

/// Human-readable metadata for a template. Surfaced in the UI palette.
#[derive(Debug, Clone, Serialize)]
pub struct TemplateDisplay {
    pub name: String,
    pub category: String,
    pub description: String,
}

impl TemplateDisplay {
    pub fn new(name: &str, category: &str, description: &str) -> Self {
        Self {
            name: name.to_string(),
            category: category.to_string(),
            description: description.to_string(),
        }
    }
}

/// Compact summary returned by `GET /api/templates` for the palette.
#[derive(Debug, Clone, Serialize)]
pub struct TemplateSummary {
    pub id: TemplateId,
    pub display: TemplateDisplay,
    pub input_ports: Vec<PortSpec>,
    pub output_ports: Vec<PortSpec>,
    pub codegen_mode: CodegenMode,
    pub debug_bridge: DebugBridgeKind,
    /// JSON Schema (as JSON Value) describing the per-instance config blob.
    /// The frontend uses this to render the config drawer form (S8+).
    pub config_schema: Value,
}

/// Plugin contract every node template implements.
///
/// New capabilities should be added as new methods with sensible defaults
/// rather than breaking changes — additive evolution is the rule.
///
/// `Send + Sync + 'static` is required because every template is shared
/// behind `Arc<dyn NodeTemplate>` in the registry, read concurrently by
/// every handler.
pub trait NodeTemplate: Send + Sync + 'static {
    fn id(&self) -> &TemplateId;
    fn display(&self) -> &TemplateDisplay;
    fn input_ports(&self) -> &[PortSpec];
    fn output_ports(&self) -> &[PortSpec];

    /// JSON Schema (produced by `schemars`) describing the per-instance
    /// `config` blob. Returning `serde_json::Value` rather than a
    /// `schemars::Schema` keeps the trait surface independent of the
    /// schemars version — the builtins compute their schema at construction
    /// time and stash the JSON.
    fn config_schema(&self) -> &Value;

    /// Validate a candidate config against the template's schema. Default
    /// impl uses `jsonschema` to compile and check; templates can override
    /// for cheaper specialised validation.
    fn validate_config(&self, config: &Value) -> Result<(), TemplateError> {
        let schema = self.config_schema();
        // Compiling per call is wasteful; the registry caches a compiled
        // validator next to each template (see `TemplateRegistry::validator_for`).
        // The default impl falls back to a per-call compile for templates
        // not added via the registry builder.
        let compiled = jsonschema::JSONSchema::compile(schema).map_err(|err| {
            TemplateError::SchemaInvalid(format!("schema for {} did not compile: {err}", self.id()))
        })?;
        if let Err(mut errs) = compiled.validate(config) {
            let first = errs
                .next()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "config did not match schema".to_string());
            return Err(TemplateError::ConfigMismatch(first));
        }
        Ok(())
    }

    /// Discriminator consumed by Section 9 (parser pack): does this
    /// template emit runtime code, codegen-time types, or both?
    fn codegen_mode(&self) -> CodegenMode {
        CodegenMode::Runtime
    }

    /// Debug-bridge contract consumed by Section 13. Default is
    /// `DebugBridgeKind::Default`; long-runners and streams override.
    fn debug_bridge(&self) -> DebugBridgeKind {
        DebugBridgeKind::Default
    }

    /// Emit the Rust source that runs at request/event time for one node
    /// instance. Default returns the `not_implemented` placeholder so
    /// built-ins with deferred codegen still compile; S4 fills in
    /// concrete bodies per template.
    fn emit_runtime(
        &self,
        _ctx: &crate::templates::codegen::CodegenCtx<'_>,
    ) -> Result<crate::templates::codegen::RuntimeEmission, TemplateError> {
        Ok(crate::templates::codegen::RuntimeEmission::not_implemented(
            self.id().as_str(),
        ))
    }

    /// Emit Rust type definitions derived from a schema file (Section 9's
    /// codegen-mode parsers). Default returns the empty schema emission;
    /// runtime-only templates never override this.
    fn emit_schema(
        &self,
        _ctx: &crate::templates::codegen::CodegenCtx<'_>,
    ) -> Result<crate::templates::codegen::SchemaEmission, TemplateError> {
        Ok(crate::templates::codegen::SchemaEmission::not_implemented(
            self.id().as_str(),
        ))
    }
}

/// Errors raised by the template subsystem. Convertible to `ApiError` via
/// the impl in `templates::error` (added in T2).
#[derive(Debug, Error)]
pub enum TemplateError {
    #[error("template id `{0}` is not registered")]
    NotFound(String),
    #[error("invalid template id: {0}")]
    InvalidId(#[from] TemplateIdError),
    #[error("config did not match template schema: {0}")]
    ConfigMismatch(String),
    #[error("template schema itself is invalid: {0}")]
    SchemaInvalid(String),
}

/// Immutable, lookup-only collection of node templates. Built once in
/// `main` via [`TemplateRegistry::with_builtins`]; never mutated after.
///
/// Wrapped in an `Arc` and threaded through Axum router state. Read paths
/// don't lock — `HashMap` lookup with `&TemplateId` key.
pub struct TemplateRegistry {
    by_id: HashMap<TemplateId, Arc<dyn NodeTemplate>>,
    /// Pre-compiled JSON-Schema validators, one per template. Avoids the
    /// per-call compile cost on the `save_graph` hot path.
    validators: HashMap<TemplateId, jsonschema::JSONSchema>,
}

impl TemplateRegistry {
    /// Empty registry — useful for tests that want to construct a controlled
    /// set of templates without dragging in every builtin.
    pub fn new() -> Self {
        Self {
            by_id: HashMap::new(),
            validators: HashMap::new(),
        }
    }

    /// Builder entry — registers built-ins. The actual list is populated in
    /// T4 (`templates::builtins::register_all`); for T1 this just returns
    /// an empty registry so the rest of the wiring compiles.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        crate::templates::builtins::register_all(&mut registry);
        registry
    }

    /// Insert a template. Returns the previous entry if the id collided
    /// (caller can panic at startup to surface duplicate-builtin bugs).
    pub fn insert(&mut self, template: Arc<dyn NodeTemplate>) -> Option<Arc<dyn NodeTemplate>> {
        let id = template.id().clone();
        // Pre-compile the validator now so save_graph doesn't pay the cost
        // per request. Compilation failure is a builtin-author bug —
        // documented expectation that schemas compile.
        match jsonschema::JSONSchema::compile(template.config_schema()) {
            Ok(compiled) => {
                self.validators.insert(id.clone(), compiled);
            }
            Err(err) => {
                tracing::error!(template = %id, ?err, "template schema failed to compile — config validation will fall back to per-call compile");
            }
        }
        self.by_id.insert(id, template)
    }

    pub fn get(&self, id: &TemplateId) -> Option<&Arc<dyn NodeTemplate>> {
        self.by_id.get(id)
    }

    pub fn contains(&self, id: &TemplateId) -> bool {
        self.by_id.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Iterate every registered template. Iteration order is unspecified.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn NodeTemplate>> {
        self.by_id.values()
    }

    /// Validate a config blob against the named template's schema using
    /// the pre-compiled validator (if available). Falls back to the
    /// trait's default `validate_config` if the registry did not cache a
    /// compiled validator.
    pub fn validate(&self, id: &TemplateId, config: &Value) -> Result<(), TemplateError> {
        let template = self
            .by_id
            .get(id)
            .ok_or_else(|| TemplateError::NotFound(id.to_string()))?;

        if let Some(compiled) = self.validators.get(id) {
            if let Err(mut errs) = compiled.validate(config) {
                let first = errs
                    .next()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "config did not match schema".to_string());
                return Err(TemplateError::ConfigMismatch(first));
            }
            return Ok(());
        }
        template.validate_config(config)
    }

    /// Snapshot every template as a wire-shaped summary list. Used by
    /// `GET /api/templates`. Order is sorted by id so the response is
    /// stable across calls.
    pub fn summaries(&self) -> Vec<TemplateSummary> {
        let mut out: Vec<_> = self
            .by_id
            .values()
            .map(|t| TemplateSummary {
                id: t.id().clone(),
                display: t.display().clone(),
                input_ports: t.input_ports().to_vec(),
                output_ports: t.output_ports().to_vec(),
                codegen_mode: t.codegen_mode(),
                debug_bridge: t.debug_bridge(),
                config_schema: t.config_schema().clone(),
            })
            .collect();
        out.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        out
    }
}

impl Default for TemplateRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// Re-export the builtins module front so `register_all` is reachable from
// `TemplateRegistry::with_builtins`. The actual module is created in T4.
pub mod builtins;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    fn id(s: &str) -> TemplateId {
        TemplateId::new(s).expect("test id must validate")
    }

    /// Minimal stub template used in registry tests so they don't depend
    /// on the builtin set.
    struct StubTemplate {
        id: TemplateId,
        display: TemplateDisplay,
        schema: Value,
        inputs: Vec<PortSpec>,
        outputs: Vec<PortSpec>,
    }

    impl StubTemplate {
        fn new(id_str: &str) -> Arc<dyn NodeTemplate> {
            Arc::new(Self {
                id: id(id_str),
                display: TemplateDisplay::new(id_str, "test", "stub template"),
                // Accept any object as config; tests can swap in stricter schemas.
                schema: json!({"type": "object"}),
                inputs: vec![],
                outputs: vec![],
            })
        }
    }

    impl NodeTemplate for StubTemplate {
        fn id(&self) -> &TemplateId { &self.id }
        fn display(&self) -> &TemplateDisplay { &self.display }
        fn input_ports(&self) -> &[PortSpec] { &self.inputs }
        fn output_ports(&self) -> &[PortSpec] { &self.outputs }
        fn config_schema(&self) -> &Value { &self.schema }
    }

    #[test]
    fn test_template_id_accepts_documented_happy_paths() {
        for s in &[
            "http.route",
            "core.dto",
            "parser.json",
            "parser.protobuf",
            "kafka.consumer",
            "abc.def.ghi",
            "a_b.c_d",
            "ns1.name2",
        ] {
            assert!(TemplateId::new(s).is_ok(), "expected {s} accepted");
        }
    }

    #[test]
    fn test_template_id_rejects_adversarial_inputs() {
        let bad: &[(&str, fn(&TemplateIdError) -> bool)] = &[
            // missing namespace (no dot)
            ("foo", |e| matches!(e, TemplateIdError::NoNamespace)),
            ("", |e| matches!(e, TemplateIdError::NoNamespace)),
            // bad chars
            ("A.b", |e| matches!(e, TemplateIdError::BadChar(_) | TemplateIdError::BadStart(_))),
            ("a-b.c", |e| matches!(e, TemplateIdError::BadChar(_))),
            ("a.b/c", |e| matches!(e, TemplateIdError::BadChar(_))),
            ("a.b c", |e| matches!(e, TemplateIdError::BadChar(_))),
            // empty segments
            (".a", |e| matches!(e, TemplateIdError::EmptySegment(_))),
            ("a.", |e| matches!(e, TemplateIdError::EmptySegment(_))),
            ("a..b", |e| matches!(e, TemplateIdError::EmptySegment(_))),
            // segment starts with non-letter
            ("1.a", |e| matches!(e, TemplateIdError::BadStart(_))),
            ("a.1", |e| matches!(e, TemplateIdError::BadStart(_))),
            ("_a.b", |e| matches!(e, TemplateIdError::BadStart(_))),
        ];
        for (raw, matcher) in bad {
            let err = TemplateId::new(raw).expect_err(&format!("expected {raw:?} rejected"));
            assert!(matcher(&err), "wrong variant for {raw:?}: {err:?}");
        }
    }

    #[test]
    fn test_template_id_length_cap() {
        let long = "a.".repeat(40); // 80 bytes — over MAX_LEN
        let err = TemplateId::new(&long).unwrap_err();
        assert!(matches!(err, TemplateIdError::TooLong(_)));
    }

    #[test]
    fn test_template_id_namespace_split() {
        assert_eq!(id("http.route").namespace(), "http");
        assert_eq!(id("parser.json").namespace(), "parser");
        assert_eq!(id("a.b.c").namespace(), "a");
    }

    #[test]
    fn test_template_id_round_trips_through_json() {
        let t = id("http.route");
        let serialised = serde_json::to_string(&t).unwrap();
        assert_eq!(serialised, "\"http.route\"");
        let back: TemplateId = serde_json::from_str(&serialised).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn test_template_id_deserialize_rejects_invalid() {
        let r: Result<TemplateId, _> = serde_json::from_str("\"BAD.id\"");
        assert!(r.is_err());
    }

    #[test]
    fn test_registry_insert_and_get() {
        let mut r = TemplateRegistry::new();
        let t = StubTemplate::new("test.first");
        assert!(r.insert(t.clone()).is_none());
        assert!(r.contains(&id("test.first")));
        assert_eq!(r.get(&id("test.first")).map(|x| x.id().as_str()), Some("test.first"));
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn test_registry_duplicate_returns_previous_entry() {
        let mut r = TemplateRegistry::new();
        let a = StubTemplate::new("dupe.id");
        let b = StubTemplate::new("dupe.id");
        assert!(r.insert(a).is_none());
        // Second insert overwrites and returns the previous Arc — caller
        // (with_builtins) panics on Some(_) to surface builtin-author bugs.
        assert!(r.insert(b).is_some());
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn test_registry_get_missing_returns_none() {
        let r = TemplateRegistry::new();
        assert!(r.get(&id("not.registered")).is_none());
    }

    #[test]
    fn test_registry_summaries_are_sorted_by_id() {
        let mut r = TemplateRegistry::new();
        r.insert(StubTemplate::new("z.one"));
        r.insert(StubTemplate::new("a.two"));
        r.insert(StubTemplate::new("m.three"));
        let summaries = r.summaries();
        assert_eq!(
            summaries.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["a.two", "m.three", "z.one"]
        );
    }

    #[test]
    fn test_registry_validate_unknown_template_id_is_not_found() {
        let r = TemplateRegistry::new();
        let err = r.validate(&id("ghost.template"), &json!({})).unwrap_err();
        assert!(matches!(err, TemplateError::NotFound(_)));
    }

    #[test]
    fn test_registry_validate_uses_template_schema() {
        let mut r = TemplateRegistry::new();
        // Stub template accepts any object — empty object is valid.
        r.insert(StubTemplate::new("test.schema"));
        assert!(r.validate(&id("test.schema"), &json!({})).is_ok());
        // Non-object should fail the `{"type":"object"}` schema.
        let err = r.validate(&id("test.schema"), &json!("not an object")).unwrap_err();
        assert!(matches!(err, TemplateError::ConfigMismatch(_)));
    }

    #[test]
    fn test_registry_default_is_empty() {
        let r = TemplateRegistry::default();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert_eq!(r.summaries().len(), 0);
    }
}
