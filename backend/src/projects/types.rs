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

/// Package slug length bounds. Same shape as project slugs — packages also
/// become filesystem directories under `projects/<slug>/packages/<pkg>/`, so
/// the same FS-safety rules apply.
const PACKAGE_SLUG_MIN_LEN: usize = 2;
const PACKAGE_SLUG_MAX_LEN: usize = 40;

/// Slug of the synthesised root package when a legacy single-graph
/// `project.json` is loaded. Stable identifier — never rename: every
/// pre-Section-1 project on disk relies on this exact value to migrate.
pub const ROOT_PACKAGE_SLUG: &str = "main";

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
        validate_slug_chars(raw, SLUG_MIN_LEN, SLUG_MAX_LEN)?;
        Ok(Slug(raw.to_owned()))
    }

    /// Borrow the underlying string. Safe to use as a filesystem path
    /// component — every byte has been validated.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Shared FS-safety validator for slug-shaped identifiers. Both [`Slug`]
/// (project names) and [`PackageSlug`] (package names inside a project)
/// route through this function so the rules cannot drift between the two
/// types — every byte that reaches the filesystem has cleared the same
/// gate.
///
/// Rules (enforced in order):
/// 1. Length in `[min, max]`.
/// 2. First byte `[a-z]`.
/// 3. Last byte `[a-z0-9]`.
/// 4. Every interior byte `[a-z0-9-]`.
/// 5. Lowercased value not in [`RESERVED_NAMES`] (Windows device names).
fn validate_slug_chars(raw: &str, min: usize, max: usize) -> Result<(), SlugError> {
    let bytes = raw.as_bytes();
    let len = bytes.len();
    if !(min..=max).contains(&len) {
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

    Ok(())
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

/// Validated package identifier within a project. Cheap to clone.
///
/// A `PackageSlug` is the name of a single folder under
/// `projects/<project-slug>/packages/<package-slug>/`. The same FS-safety
/// rules apply as for project [`Slug`]s — `PackageSlug` exists as a
/// separate type purely to prevent compile-time confusion: a function
/// expecting a `PackageSlug` cannot accidentally receive a `Slug` (which
/// names the *enclosing* project) and vice versa.
///
/// Construct via [`PackageSlug::new`] or `parse()`/`FromStr` / `TryFrom`.
/// `Deserialize` routes through the validator.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PackageSlug(String);

impl PackageSlug {
    /// Validate `raw` against the package-slug ruleset and return an owned
    /// `PackageSlug`. Length bounds are
    /// `[PACKAGE_SLUG_MIN_LEN, PACKAGE_SLUG_MAX_LEN]`; all other rules are
    /// shared with [`Slug`] via [`validate_slug_chars`].
    pub fn new(raw: &str) -> Result<Self, SlugError> {
        validate_slug_chars(raw, PACKAGE_SLUG_MIN_LEN, PACKAGE_SLUG_MAX_LEN)?;
        Ok(PackageSlug(raw.to_owned()))
    }

    /// Borrow the underlying string. Safe to use as a filesystem path
    /// component — every byte has been validated.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The canonical root package slug (`"main"`). Used by the legacy-shape
    /// project migration to synthesise a one-package tree from a
    /// single-graph `project.json`.
    pub fn root() -> Self {
        // Safe: `"main"` is a 4-char ASCII lowercase string that passes
        // every rule. Constructed here rather than via `new` so the hot
        // legacy-migration path never reaches the validator.
        PackageSlug(ROOT_PACKAGE_SLUG.to_string())
    }
}

impl fmt::Display for PackageSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for PackageSlug {
    type Err = SlugError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PackageSlug::new(s)
    }
}

impl TryFrom<&str> for PackageSlug {
    type Error = SlugError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        PackageSlug::new(value)
    }
}

impl TryFrom<String> for PackageSlug {
    type Error = SlugError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        PackageSlug::new(&value)
    }
}

// Same closed-deser pattern as `Slug` — malformed package slugs fail at
// JSON deserialise time so handler code never sees an invalid value.
impl<'de> Deserialize<'de> for PackageSlug {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        PackageSlug::new(&raw).map_err(serde::de::Error::custom)
    }
}

/// Stable opaque identifier for a [`Package`]. The studio assigns UUIDs at
/// create time; this type carries no validator because the id never
/// reaches the filesystem (only the slug does).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PackageId(pub String);

/// One node in a project's package tree.
///
/// A package corresponds 1:1 with a Rust module under `src/<path>/mod.rs`.
/// The tree shape is fully user-controlled — the studio neither
/// prescribes nor presupposes any layout (DDD, hexagonal, flat, library,
/// workspace, etc. are all valid).
///
/// Invariants enforced by [`Project::validate_package_tree`]:
/// 1. Exactly one root (`parent_id == None`).
/// 2. No duplicate package ids.
/// 3. Sibling slugs are unique (so the `src/<path>/` segment is
///    unambiguous).
/// 4. Every `parent_id` resolves to an existing package id.
/// 5. The parent chain is acyclic (a DFS from the root reaches every
///    package).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Package {
    pub id: PackageId,
    pub slug: PackageSlug,
    /// `None` for the root package; otherwise points to the parent
    /// package's [`PackageId`].
    pub parent_id: Option<PackageId>,
    /// Optional user-facing label shown in the package-tree sidebar. The
    /// UI falls back to `slug` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Reasons a package tree failed structural validation. Surfaced server-side
/// via logs; the client gets a sanitised summary through `ApiError`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PackageTreeError {
    #[error("package tree must contain at least one package")]
    Empty,
    #[error("package tree must have exactly one root (parent_id == None); found {0}")]
    RootCount(usize),
    #[error("duplicate package id: {0}")]
    DuplicateId(String),
    #[error("duplicate slug {slug} under parent {parent:?}")]
    DuplicateSiblingSlug { slug: String, parent: Option<String> },
    #[error("package {child} references non-existent parent id {parent}")]
    DanglingParent { child: String, parent: String },
    #[error("cycle detected in package tree involving id {0}")]
    Cycle(String),
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

/// Stable, recognisable [`PackageId`] for the root package synthesised when
/// a legacy single-graph `project.json` is migrated to the multi-package
/// shape. New roots created by users get fresh UUIDs; this literal is
/// reserved for the migration path so support staff can identify
/// pre-Section-1 projects at a glance.
pub const LEGACY_ROOT_PACKAGE_ID: &str = "pkg-root";

/// Default value for [`Project::packages`] when the field is absent on
/// disk (pre-Section-1 single-graph projects). Synthesises a one-package
/// tree with a root named `"main"` so existing `graph.json` data can be
/// hoisted into `packages/main/graph.json` on the first save (the disk
/// migration runs at the store layer in T2).
fn default_root_packages() -> Vec<Package> {
    vec![Package {
        id: PackageId(LEGACY_ROOT_PACKAGE_ID.to_string()),
        slug: PackageSlug::root(),
        parent_id: None,
        label: None,
    }]
}

/// Full project document persisted at `projects/<slug>/project.json`.
///
/// Graph data is NOT inlined here — each package has its own
/// `packages/<pkg-slug>/graph.json` file so list endpoints and metadata
/// edits never read large graph blobs.
///
/// ## On-disk shape (current)
///
/// ```json
/// {
///   "slug": "...", "name": "...", "created_at": "...",
///   "updated_at": "...", "schema_version": 1,
///   "packages": [
///     { "id": "...", "slug": "main", "parent_id": null }
///   ]
/// }
/// ```
///
/// ## Legacy shape (pre-Section-1 single-graph projects)
///
/// `packages` field absent. The custom `default` synthesises a one-root
/// tree (`slug = "main"`, `id = "pkg-root"`) so legacy files round-trip
/// without loss. The next save by the store layer writes the canonical
/// shape; the disk-side `graph.json` → `packages/main/graph.json` hoist
/// runs in T2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    #[serde(flatten)]
    pub meta: ProjectMeta,
    /// User-defined package tree. Never empty in valid documents; on
    /// legacy load, [`default_root_packages`] supplies a single root.
    /// Structural validity is verified by
    /// [`Project::validate_package_tree`] before the project is handed to
    /// any caller; the field is left `pub` for ergonomic construction
    /// in tests and migrations only.
    #[serde(default = "default_root_packages")]
    pub packages: Vec<Package>,
}

impl Project {
    /// Verify the package tree satisfies all five structural invariants
    /// documented on [`Package`]. Pure function over `self.packages`.
    ///
    /// Complexity: O(n) for the id-uniqueness, root-count, dangling-
    /// parent, and sibling-slug checks; O(n × depth) for the cycle walk
    /// (each node walks up to its root). Worst case for a degenerate
    /// linear chain of N packages is O(n²); typical projects carry
    /// dozens of packages, so the quadratic term is dominated by
    /// constants in practice.
    ///
    /// Call this:
    /// - After loading a `project.json` from disk, before exposing the
    ///   project to handler code.
    /// - Before writing any user-driven mutation (create / rename /
    ///   delete package) so a malformed tree never reaches disk.
    pub fn validate_package_tree(&self) -> Result<(), PackageTreeError> {
        use std::collections::{HashMap, HashSet};

        if self.packages.is_empty() {
            return Err(PackageTreeError::Empty);
        }

        // Invariant 2: unique ids. Build the id→parent map alongside so
        // we don't pay a second pass.
        let mut id_to_parent: HashMap<&PackageId, &Option<PackageId>> =
            HashMap::with_capacity(self.packages.len());
        for p in &self.packages {
            if id_to_parent.insert(&p.id, &p.parent_id).is_some() {
                return Err(PackageTreeError::DuplicateId(p.id.0.clone()));
            }
        }

        // Invariant 1: exactly one root.
        let root_count = self.packages.iter().filter(|p| p.parent_id.is_none()).count();
        if root_count != 1 {
            return Err(PackageTreeError::RootCount(root_count));
        }

        // Invariant 4: every parent_id resolves.
        for p in &self.packages {
            if let Some(parent) = &p.parent_id {
                if !id_to_parent.contains_key(parent) {
                    return Err(PackageTreeError::DanglingParent {
                        child: p.id.0.clone(),
                        parent: parent.0.clone(),
                    });
                }
            }
        }

        // Invariant 3: sibling slug uniqueness. Key is (parent_id_str,
        // slug_str) — siblings share a parent and must not collide.
        let mut sibling_keys: HashSet<(Option<&str>, &str)> =
            HashSet::with_capacity(self.packages.len());
        for p in &self.packages {
            let key = (
                p.parent_id.as_ref().map(|pid| pid.0.as_str()),
                p.slug.as_str(),
            );
            if !sibling_keys.insert(key) {
                return Err(PackageTreeError::DuplicateSiblingSlug {
                    slug: p.slug.0.clone(),
                    parent: p.parent_id.as_ref().map(|pid| pid.0.clone()),
                });
            }
        }

        // Invariant 5: acyclic. Walk each node up to the root; if we revisit
        // any id in a single walk, there's a cycle. Bound the walk by the
        // total node count so a malicious tree can't loop forever.
        for p in &self.packages {
            let mut seen: HashSet<&PackageId> = HashSet::new();
            let mut cursor: Option<&PackageId> = p.parent_id.as_ref();
            let mut steps = 0usize;
            while let Some(pid) = cursor {
                if !seen.insert(pid) || steps > self.packages.len() {
                    return Err(PackageTreeError::Cycle(p.id.0.clone()));
                }
                cursor = id_to_parent
                    .get(pid)
                    .and_then(|parent_opt| parent_opt.as_ref());
                steps += 1;
            }
        }

        Ok(())
    }
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

    // ----- PackageSlug -----

    #[test]
    fn test_package_slug_shares_rules_with_slug() {
        // Same validator means the same accepted forms.
        assert!(PackageSlug::new("main").is_ok());
        assert!(PackageSlug::new("auth-store").is_ok());
        assert!(PackageSlug::new("a1").is_ok());

        // Same rejections.
        assert!(matches!(PackageSlug::new(""), Err(SlugError::Length(0))));
        assert!(matches!(PackageSlug::new("A"), Err(SlugError::Length(1))));
        assert!(matches!(PackageSlug::new("Main"), Err(SlugError::BadStart)));
        assert!(matches!(PackageSlug::new("main-"), Err(SlugError::BadEnd)));
        assert!(matches!(PackageSlug::new("a/b"), Err(SlugError::BadChar(_))));
        assert!(matches!(PackageSlug::new(".."), Err(SlugError::BadStart)));
        assert!(matches!(PackageSlug::new("con"), Err(SlugError::Reserved)));
    }

    #[test]
    fn test_package_slug_root_is_main() {
        // `root()` must produce the exact byte sequence that the legacy
        // disk-migration path uses. Renaming this would orphan every
        // pre-Section-1 project.
        assert_eq!(PackageSlug::root().as_str(), "main");
        assert_eq!(PackageSlug::root().as_str(), ROOT_PACKAGE_SLUG);
    }

    #[test]
    fn test_package_slug_round_trips_through_json() {
        let s = PackageSlug::new("user-service").unwrap();
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, "\"user-service\"");
        let back: PackageSlug = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn test_package_slug_deserialize_rejects_invalid_payloads() {
        // Closed-deser: a malformed slug arriving in a JSON request body
        // must fail at deserialise time, not later.
        let r: Result<PackageSlug, _> = serde_json::from_str("\"BadCase\"");
        assert!(r.is_err());
        let r: Result<PackageSlug, _> = serde_json::from_str("\"con\"");
        assert!(r.is_err());
    }

    // ----- Project package tree -----

    fn pkg(id: &str, slug: &str, parent: Option<&str>) -> Package {
        Package {
            id: PackageId(id.to_string()),
            slug: PackageSlug::new(slug).unwrap(),
            parent_id: parent.map(|s| PackageId(s.to_string())),
            label: None,
        }
    }

    fn project_with_packages(packages: Vec<Package>) -> Project {
        Project {
            meta: ProjectMeta {
                slug: Slug::new("demo").unwrap(),
                name: "Demo".into(),
                created_at: OffsetDateTime::UNIX_EPOCH,
                updated_at: OffsetDateTime::UNIX_EPOCH,
                schema_version: PROJECT_SCHEMA_VERSION,
            },
            packages,
        }
    }

    #[test]
    fn test_project_legacy_json_synthesises_root_package() {
        // Pre-Section-1 project.json has no `packages` field. The default
        // must lift it into a one-root tree so the next save can write
        // the canonical shape (and T2's disk migration can hoist
        // graph.json into packages/main/).
        let legacy = serde_json::json!({
            "slug": "demo",
            "name": "Demo",
            "created_at": "1970-01-01T00:00:00Z",
            "updated_at": "1970-01-01T00:00:00Z",
            "schema_version": 1
        });
        let p: Project = serde_json::from_value(legacy).unwrap();
        assert_eq!(p.packages.len(), 1);
        assert!(p.packages[0].parent_id.is_none());
        assert_eq!(p.packages[0].slug.as_str(), "main");
        assert_eq!(p.packages[0].id.0, LEGACY_ROOT_PACKAGE_ID);
        p.validate_package_tree()
            .expect("synthesised root must be a valid tree");
    }

    #[test]
    fn test_project_canonical_json_preserves_packages() {
        let canonical = serde_json::json!({
            "slug": "demo",
            "name": "Demo",
            "created_at": "1970-01-01T00:00:00Z",
            "updated_at": "1970-01-01T00:00:00Z",
            "schema_version": 1,
            "packages": [
                { "id": "r", "slug": "core", "parent_id": null },
                { "id": "c", "slug": "auth", "parent_id": "r" }
            ]
        });
        let p: Project = serde_json::from_value(canonical).unwrap();
        assert_eq!(p.packages.len(), 2);
        assert_eq!(p.packages[0].id.0, "r");
        assert_eq!(p.packages[1].parent_id.as_ref().unwrap().0, "r");
        p.validate_package_tree().unwrap();
    }

    #[test]
    fn test_validate_package_tree_rejects_empty() {
        let p = project_with_packages(vec![]);
        assert_eq!(p.validate_package_tree(), Err(PackageTreeError::Empty));
    }

    #[test]
    fn test_validate_package_tree_rejects_zero_or_multiple_roots() {
        // Zero roots: every node has a parent so the tree has no entry
        // point. The validator's root-count check fires regardless of
        // whether the chain is also cyclic.
        let no_root = project_with_packages(vec![
            pkg("a", "alpha", Some("b")),
            pkg("b", "beta", Some("a")),
        ]);
        assert_eq!(
            no_root.validate_package_tree(),
            Err(PackageTreeError::RootCount(0))
        );

        // Two roots: ambiguous which is the project entry point.
        let two_roots = project_with_packages(vec![
            pkg("a", "alpha", None),
            pkg("b", "beta", None),
        ]);
        assert_eq!(
            two_roots.validate_package_tree(),
            Err(PackageTreeError::RootCount(2))
        );
    }

    #[test]
    fn test_validate_package_tree_rejects_duplicate_ids() {
        let p = project_with_packages(vec![
            pkg("a", "alpha", None),
            pkg("a", "beta", Some("a")),
        ]);
        assert_eq!(
            p.validate_package_tree(),
            Err(PackageTreeError::DuplicateId("a".into()))
        );
    }

    #[test]
    fn test_validate_package_tree_rejects_sibling_slug_collision() {
        // Two children of the same parent can't share a slug — they would
        // claim the same `src/<parent>/<slug>/` folder.
        let p = project_with_packages(vec![
            pkg("r", "root", None),
            pkg("c1", "child", Some("r")),
            pkg("c2", "child", Some("r")),
        ]);
        match p.validate_package_tree() {
            Err(PackageTreeError::DuplicateSiblingSlug { slug, parent }) => {
                assert_eq!(slug, "child");
                assert_eq!(parent.as_deref(), Some("r"));
            }
            other => panic!("expected DuplicateSiblingSlug, got {other:?}"),
        }
    }

    #[test]
    fn test_validate_package_tree_allows_same_slug_under_different_parents() {
        // Two cousins both named `util` is fine — they map to different
        // module paths (`a::util` and `b::util`).
        let p = project_with_packages(vec![
            pkg("r", "root", None),
            pkg("a", "alpha", Some("r")),
            pkg("b", "beta", Some("r")),
            pkg("au", "util", Some("a")),
            pkg("bu", "util", Some("b")),
        ]);
        p.validate_package_tree().unwrap();
    }

    #[test]
    fn test_validate_package_tree_rejects_dangling_parent() {
        let p = project_with_packages(vec![
            pkg("r", "root", None),
            pkg("c", "child", Some("ghost")),
        ]);
        match p.validate_package_tree() {
            Err(PackageTreeError::DanglingParent { child, parent }) => {
                assert_eq!(child, "c");
                assert_eq!(parent, "ghost");
            }
            other => panic!("expected DanglingParent, got {other:?}"),
        }
    }

    #[test]
    fn test_validate_package_tree_rejects_cycle() {
        // A valid root plus a two-node cycle alongside it. Root-count is
        // satisfied (exactly one None parent), ids are unique, every
        // parent resolves — so the only remaining defence is the cycle
        // walk, which must catch the a→b→a loop.
        let p = project_with_packages(vec![
            pkg("r", "root", None),
            pkg("a", "alpha", Some("b")),
            pkg("b", "beta", Some("a")),
        ]);
        match p.validate_package_tree() {
            Err(PackageTreeError::Cycle(_)) => {}
            other => panic!("expected Cycle, got {other:?}"),
        }
    }

    #[test]
    fn test_project_round_trip_preserves_package_tree() {
        let original = project_with_packages(vec![
            pkg("r", "root", None),
            pkg("a", "auth", Some("r")),
            pkg("s", "store", Some("r")),
        ]);
        let json = serde_json::to_string(&original).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.packages, original.packages);
        back.validate_package_tree().unwrap();
    }
}
