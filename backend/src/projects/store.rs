//! Filesystem-backed project store.
//!
//! `ProjectStore` owns the `projects/` root and serves every CRUD verb the
//! HTTP layer needs. Two correctness invariants drive the design:
//!
//! 1. **Atomicity of `save_graph`.** A crash mid-write must never leave
//!    `graph.json` in a half-written state — that would brick the project.
//!    Every write goes via the `tmp + rename` pattern: write the new bytes
//!    to `graph.json.tmp` sibling, `fsync` it, then `rename` to `graph.json`.
//!    POSIX guarantees `rename` is atomic for same-filesystem moves, and the
//!    sibling tmp file is in the same directory as the target by construction.
//!
//! 2. **Serialisation of concurrent writes on the same slug.** Two PUT
//!    requests racing on the same project must not interleave — but two PUTs
//!    on different projects must not block each other. Implemented with a
//!    `DashMap<Slug, Arc<tokio::sync::Mutex<()>>>` of per-slug locks, lazily
//!    populated. The map grows monotonically; at ~40 bytes/slug it is not
//!    worth bounding for v1 and is documented as such.
//!
//! `Slug` is the security boundary — every path join goes through a value
//! that has been validated by `Slug::new`, so no user input ever reaches a
//! `Path` without character-class filtering and length bounds.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use serde::Serialize;
use time::OffsetDateTime;
use tokio::fs;
use tokio::sync::Mutex;
use tracing::warn;
use uuid::Uuid;

use crate::error::ApiError;
use crate::projects::types::{
    Graph, Project, ProjectMeta, Slug, GRAPH_SCHEMA_VERSION, PROJECT_SCHEMA_VERSION,
};

const PROJECT_FILE: &str = "project.json";
const GRAPH_FILE: &str = "graph.json";
const TMP_SUFFIX: &str = ".tmp";

/// Owner of every persisted project on disk.
///
/// Cheap to clone — the internal state is reference-counted (`DashMap` and
/// the per-slug `Arc<Mutex<_>>`s). Construct once in `main` and share via
/// Axum's router `with_state`.
#[derive(Clone)]
pub struct ProjectStore {
    root: PathBuf,
    /// Per-slug write locks, lazily created on first acquisition.
    /// Reads (load / load_graph / list) do not acquire — only writes do.
    locks: Arc<DashMap<Slug, Arc<Mutex<()>>>>,
}

impl ProjectStore {
    /// Construct a store rooted at `root`. The directory is created if it
    /// does not exist (idempotent; existing root is reused).
    pub async fn new(root: impl Into<PathBuf>) -> Result<Self, ApiError> {
        let root = root.into();
        fs::create_dir_all(&root).await?;
        Ok(Self {
            root,
            locks: Arc::new(DashMap::new()),
        })
    }

    /// Read-only accessor for the root path — used by tests + diagnostics.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn project_dir(&self, slug: &Slug) -> PathBuf {
        // Safe because `Slug` is the only type permitted here and its
        // validator forbids `..`, `/`, `\`, NUL, and control characters.
        self.root.join(slug.as_str())
    }

    /// Acquire (or create) the per-slug write lock. Returns a guard the
    /// caller holds for the duration of the critical section.
    fn lock_for(&self, slug: &Slug) -> Arc<Mutex<()>> {
        // Use the entry API so two concurrent first-time acquirers cannot
        // race on insert.
        self.locks
            .entry(slug.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Create a new project with the given slug and display name.
    ///
    /// Atomic on the directory boundary: `tokio::fs::create_dir` fails with
    /// `AlreadyExists` if another caller (or a previous run) already
    /// created the folder. This is the *only* AlreadyExists barrier — we
    /// never rely on a check-then-act sequence.
    pub async fn create(&self, slug: Slug, name: String) -> Result<Project, ApiError> {
        let dir = self.project_dir(&slug);
        match fs::create_dir(&dir).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(ApiError::AlreadyExists);
            }
            Err(err) => return Err(err.into()),
        }

        let now = OffsetDateTime::now_utc();
        let meta = ProjectMeta {
            slug: slug.clone(),
            name,
            created_at: now,
            updated_at: now,
            schema_version: PROJECT_SCHEMA_VERSION,
        };
        let project = Project { meta: meta.clone() };

        // Use the same atomic write helper that save_graph uses, so a crash
        // between create_dir and the project.json write leaves the dir but
        // no project.json — recoverable on retry (create() will see the
        // dir, hit AlreadyExists, and the operator can delete it manually).
        // Acceptable for v1.
        write_json_atomic(&dir.join(PROJECT_FILE), &project).await?;

        // Initialise the graph with a single entry-point node at the centre
        // of the canvas so the first screen the developer sees is main.rs
        // in visual form — the root from which the flow begins.
        let mut initial_graph = Graph::default();
        initial_graph.nodes.push(crate::projects::types::Node {
            id: crate::projects::types::NodeId("entry".into()),
            template_id: crate::templates::TemplateId::new("core.entry_point").unwrap(),
            position: crate::projects::types::Position { x: 0.0, y: 0.0 },
            config: serde_json::json!({"bind_address": "127.0.0.1:8080", "log_level": "info"}),
            label: Some("main.rs".into()),
            comment: None,
        });
        write_json_atomic(&dir.join(GRAPH_FILE), &initial_graph).await?;

        Ok(project)
    }

    /// List all projects (just their metadata headers — graph bodies are
    /// not loaded). Silently ignores filesystem entries whose name fails
    /// slug validation or whose `project.json` is missing/malformed; those
    /// are logged but not propagated, so a single junk folder does not
    /// break the list endpoint.
    pub async fn list(&self) -> Result<Vec<ProjectMeta>, ApiError> {
        let mut out = Vec::new();
        let mut dir = fs::read_dir(&self.root).await?;
        while let Some(entry) = dir.next_entry().await? {
            let file_type = entry.file_type().await?;
            if !file_type.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                warn!(?name, "skipping non-utf8 entry in projects root");
                continue;
            };
            let Ok(slug) = Slug::new(name_str) else {
                // Junk folder (`.scratch`, `tmp-XYZ`, etc.) — ignore.
                continue;
            };
            match self.load(&slug).await {
                Ok(p) => out.push(p.meta),
                Err(err) => {
                    warn!(slug = %slug, ?err, "skipping unreadable project");
                }
            }
        }
        // Stable order — sort by created_at desc so the newest project
        // surfaces first in the UI list.
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    /// Load the full project document (metadata only — graph is separate).
    pub async fn load(&self, slug: &Slug) -> Result<Project, ApiError> {
        let path = self.project_dir(slug).join(PROJECT_FILE);
        read_json(&path).await
    }

    /// Delete the project's folder and everything in it.
    ///
    /// This is destructive and irreversible from the studio's side. The
    /// studio's `.history/` audit trail captures regenerations, not
    /// project deletions — git is the user's safety net if they wired one.
    pub async fn delete(&self, slug: &Slug) -> Result<(), ApiError> {
        let dir = self.project_dir(slug);
        match fs::remove_dir_all(&dir).await {
            Ok(()) => {
                // Drop the per-slug lock entry so future creates with the
                // same slug start fresh.
                self.locks.remove(slug);
                Ok(())
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(ApiError::NotFound),
            Err(err) => Err(err.into()),
        }
    }

    /// Load the project's flow graph.
    pub async fn load_graph(&self, slug: &Slug) -> Result<Graph, ApiError> {
        let path = self.project_dir(slug).join(GRAPH_FILE);
        read_json(&path).await
    }

    /// Persist a new flow graph for the project. Atomic on the file
    /// boundary via `tmp + rename`. Serialised against other writes on the
    /// same slug via the per-slug `Mutex`; concurrent writes on different
    /// slugs do not block each other.
    ///
    /// S3+: every node in the graph is validated against the template
    /// registry before the file is touched. Two checks per node — the
    /// `template_id` must be registered, and the `config` blob must
    /// validate against the template's JSON Schema. The first failure
    /// returns `InvalidGraph` with the underlying detail in the server
    /// log; the client sees the sanitised envelope.
    pub async fn save_graph(
        &self,
        slug: &Slug,
        graph: &Graph,
        registry: &crate::templates::TemplateRegistry,
    ) -> Result<(), ApiError> {
        // Validate the schema version at the boundary. S3+ may bump this;
        // for v1 the only accepted value is the current constant.
        if graph.schema_version != GRAPH_SCHEMA_VERSION {
            return Err(ApiError::InvalidGraph(format!(
                "unsupported graph schema_version {}, expected {}",
                graph.schema_version, GRAPH_SCHEMA_VERSION
            )));
        }

        // Validate every node against the registry BEFORE filesystem I/O.
        // Iteration order is stable (Vec order from the wire), so the
        // "first failure" the client sees corresponds to the first node
        // in the graph that breaks the contract.
        for node in &graph.nodes {
            registry.validate(&node.template_id, &node.config)?;
        }

        // Validate graph structure — edges must reference real nodes and
        // declared ports, and type tags must be compatible.
        if let Err(errors) = crate::projects::validation::validate_graph(graph, registry) {
            let messages: Vec<String> = errors.iter().map(|e| e.message()).collect();
            return Err(ApiError::InvalidGraph(messages.join("; ")));
        }

        let dir = self.project_dir(slug);
        // Existence probe: if the project dir doesn't exist, treat as NotFound
        // (don't autocreate — that would let a stray PUT resurrect a deleted
        // project, which is surprising behaviour).
        match fs::metadata(&dir).await {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(ApiError::NotFound);
            }
            Err(err) => return Err(err.into()),
        }

        let lock = self.lock_for(slug);
        let _guard = lock.lock().await;

        write_json_atomic(&dir.join(GRAPH_FILE), graph).await?;

        // Bump updated_at on the project metadata. Best-effort: if the
        // metadata file is missing (corruption / external delete), log and
        // proceed — the graph itself is persisted.
        match read_json::<Project>(&dir.join(PROJECT_FILE)).await {
            Ok(mut project) => {
                project.meta.updated_at = OffsetDateTime::now_utc();
                if let Err(err) = write_json_atomic(&dir.join(PROJECT_FILE), &project).await {
                    warn!(slug = %slug, ?err, "failed to refresh project metadata after graph save");
                }
            }
            Err(err) => {
                warn!(slug = %slug, ?err, "could not read project metadata to bump updated_at");
            }
        }

        Ok(())
    }
}

/// Read a JSON document, mapping `NotFound` to `ApiError::NotFound`.
///
/// Free function rather than a method so it can be reused for both the
/// project metadata and the graph file without duplicating I/O glue.
async fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, ApiError> {
    let bytes = match fs::read(path).await {
        Ok(b) => b,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(ApiError::NotFound);
        }
        Err(err) => return Err(err.into()),
    };
    let value = serde_json::from_slice(&bytes)?;
    Ok(value)
}

/// Write a JSON document atomically using the `tmp + rename` pattern.
///
/// Steps:
/// 1. Serialise to bytes (pretty-printed for human-editable graph.json).
/// 2. Write to `<target>.tmp.<uuid>`. The UUID guards against two writers
///    racing on the SAME slug with broken locking (defense in depth) and
///    against a stale tmp from a prior crash.
/// 3. Sync the file's contents to disk so a power loss between write and
///    rename does not lose the data.
/// 4. `rename` the tmp file onto the target. POSIX guarantees this is
///    atomic for same-filesystem moves; the tmp lives in the same dir as
///    the target by construction.
async fn write_json_atomic<T: Serialize>(target: &Path, value: &T) -> Result<(), ApiError> {
    let bytes = serde_json::to_vec_pretty(value)?;

    let parent = target
        .parent()
        .ok_or_else(|| ApiError::Internal(format!("target path has no parent: {target:?}")))?;
    let tmp_name = format!(
        "{}{}{}",
        target
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("write"),
        TMP_SUFFIX,
        Uuid::new_v4()
    );
    let tmp_path = parent.join(tmp_name);

    {
        let mut f = fs::File::create(&tmp_path).await?;
        use tokio::io::AsyncWriteExt;
        f.write_all(&bytes).await?;
        f.flush().await?;
        f.sync_all().await?;
    }

    // Atomic publish. If this fails, clean up the tmp file so we don't
    // leave litter on disk. (`rename` failure modes: target on a different
    // filesystem [EXDEV — impossible by construction], permission denied,
    // or a concurrent unlink of the parent dir — all genuinely exceptional.)
    if let Err(err) = fs::rename(&tmp_path, target).await {
        let _ = fs::remove_file(&tmp_path).await;
        return Err(err.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn slug(s: &str) -> Slug {
        Slug::new(s).expect("test slug should validate")
    }

    /// Pre-built registry with every studio built-in. Construct per-test
    /// (cheap) so each case is independent.
    fn registry() -> crate::templates::TemplateRegistry {
        crate::templates::TemplateRegistry::with_builtins()
    }

    #[tokio::test]
    async fn test_create_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        let p = store
            .create(slug("user-service"), "User service".into())
            .await
            .unwrap();
        let loaded = store.load(&slug("user-service")).await.unwrap();
        assert_eq!(loaded.meta.slug, p.meta.slug);
        assert_eq!(loaded.meta.name, "User service");
        assert_eq!(loaded.meta.schema_version, PROJECT_SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn test_create_then_create_same_slug_yields_already_exists() {
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        store.create(slug("dupe"), "first".into()).await.unwrap();
        let err = store.create(slug("dupe"), "second".into()).await.unwrap_err();
        assert!(matches!(err, ApiError::AlreadyExists));
    }

    #[tokio::test]
    async fn test_load_missing_project_is_not_found() {
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        let err = store.load(&slug("ghost")).await.unwrap_err();
        assert!(matches!(err, ApiError::NotFound));
    }

    #[tokio::test]
    async fn test_delete_then_load_is_not_found() {
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        store.create(slug("doomed"), "x".into()).await.unwrap();
        store.delete(&slug("doomed")).await.unwrap();
        assert!(matches!(
            store.load(&slug("doomed")).await,
            Err(ApiError::NotFound)
        ));
    }

    #[tokio::test]
    async fn test_delete_missing_is_not_found() {
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        let err = store.delete(&slug("never-was")).await.unwrap_err();
        assert!(matches!(err, ApiError::NotFound));
    }

    #[tokio::test]
    async fn test_list_returns_only_valid_projects_sorted_newest_first() {
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        // Junk folder that fails slug validation — must be ignored, not crash.
        fs::create_dir(dir.path().join(".scratch")).await.unwrap();
        // Folder with valid slug but missing project.json — must be skipped.
        fs::create_dir(dir.path().join("orphan-dir")).await.unwrap();

        store.create(slug("alpha"), "Alpha".into()).await.unwrap();
        // Sleep a microsecond so created_at strictly orders.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        store.create(slug("beta"), "Beta".into()).await.unwrap();

        let list = store.list().await.unwrap();
        assert_eq!(list.len(), 2, "junk and orphan must be skipped");
        assert_eq!(list[0].slug.as_str(), "beta", "newest first");
        assert_eq!(list[1].slug.as_str(), "alpha");
    }

    #[tokio::test]
    async fn test_save_graph_round_trips_and_bumps_updated_at() {
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        let p = store
            .create(slug("flow"), "Flow".into())
            .await
            .unwrap();
        // Force a measurable time gap so updated_at changes observably.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let mut g = Graph::default();
        // Insert a node and edge with synthetic ids; the store doesn't
        // interpret them at v1, only persists.
        g.nodes.push(crate::projects::types::Node {
            id: crate::projects::types::NodeId("n1".into()),
            template_id: crate::templates::TemplateId::new("http.route").unwrap(),
            position: crate::projects::types::Position { x: 10.0, y: 20.0 },
            config: serde_json::json!({"path": "/", "method": "GET"}),
            label: Some("root".into()),
            comment: None,
        });

        store.save_graph(&slug("flow"), &g, &registry()).await.unwrap();

        let back = store.load_graph(&slug("flow")).await.unwrap();
        assert_eq!(back.nodes.len(), 1);
        assert_eq!(back.nodes[0].id.0, "n1");
        let p2 = store.load(&slug("flow")).await.unwrap();
        assert!(p2.meta.updated_at > p.meta.updated_at, "updated_at must advance");
    }

    #[tokio::test]
    async fn test_save_graph_rejects_unknown_schema_version() {
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        store.create(slug("ver"), "v".into()).await.unwrap();
        let mut g = Graph::default();
        g.schema_version = 999;
        let err = store
            .save_graph(&slug("ver"), &g, &registry())
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::InvalidGraph(_)));
    }

    #[tokio::test]
    async fn test_save_graph_on_missing_project_is_not_found() {
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        let err = store
            .save_graph(&slug("nope"), &Graph::default(), &registry())
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::NotFound));
    }

    #[tokio::test]
    async fn test_save_graph_rejects_unknown_template_id() {
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        store.create(slug("badt"), "x".into()).await.unwrap();
        let mut g = Graph::default();
        g.nodes.push(crate::projects::types::Node {
            id: crate::projects::types::NodeId("n1".into()),
            template_id: crate::templates::TemplateId::new("ghost.template").unwrap(),
            position: crate::projects::types::Position { x: 0.0, y: 0.0 },
            config: serde_json::Value::Null,
            label: None,
            comment: None,
        });
        let err = store.save_graph(&slug("badt"), &g, &registry()).await.unwrap_err();
        assert!(matches!(err, ApiError::InvalidGraph(_)));
    }

    #[tokio::test]
    async fn test_save_graph_rejects_bad_node_config() {
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        store.create(slug("badc"), "x".into()).await.unwrap();
        let mut g = Graph::default();
        // http.route requires {path, method}; supplying neither must fail.
        g.nodes.push(crate::projects::types::Node {
            id: crate::projects::types::NodeId("n1".into()),
            template_id: crate::templates::TemplateId::new("http.route").unwrap(),
            position: crate::projects::types::Position { x: 0.0, y: 0.0 },
            config: serde_json::json!({}),
            label: None,
            comment: None,
        });
        let err = store.save_graph(&slug("badc"), &g, &registry()).await.unwrap_err();
        assert!(matches!(err, ApiError::InvalidGraph(_)));
    }

    #[tokio::test]
    async fn test_concurrent_saves_on_same_slug_produce_consistent_state() {
        // 16 concurrent writers, each with a distinct node id. The final
        // graph.json must be deserialisable (no torn JSON) and must equal
        // exactly one of the writers' inputs (last-writer-wins under the
        // per-slug Mutex).
        let dir = tempdir().unwrap();
        let store = Arc::new(ProjectStore::new(dir.path()).await.unwrap());
        store.create(slug("hot"), "hot".into()).await.unwrap();

        let mut handles = Vec::new();
        for i in 0..16u32 {
            let store = store.clone();
            handles.push(tokio::spawn(async move {
                let mut g = Graph::default();
                g.nodes.push(crate::projects::types::Node {
                    id: crate::projects::types::NodeId(format!("n{i}")),
                    template_id: crate::templates::TemplateId::new("core.dto").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"name": "X", "fields": []}),
                    label: None,
                    comment: None,
                });
                let reg = registry();
                store.save_graph(&slug("hot"), &g, &reg).await
            }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }

        // Must deserialise cleanly (no torn JSON) and have exactly one node.
        let final_graph = store.load_graph(&slug("hot")).await.unwrap();
        assert_eq!(final_graph.nodes.len(), 1, "exactly one writer's state survives");
    }
}
