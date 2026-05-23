//! In-memory cache of "last regenerated graph hash" per project.
//!
//! The Build / Run / Test / Debug endpoints call `regen_if_changed` before
//! spawning cargo. On an unchanged graph the cache short-circuits the whole
//! codegen pass — handler returns immediately with `regenerated: false`.
//!
//! The cache is purely an optimisation: the underlying generator
//! (`files::write_atomic_if_changed`) is already file-level idempotent, so a
//! cache miss on the same graph would still produce zero changed files. The
//! cache saves the template-emission + format pass on repeat clicks, which
//! is the only non-trivial cost in a same-graph regen.
//!
//! ## Hash determinism
//!
//! The hash is computed over `serde_json::to_vec(graph)`. The default
//! `serde_json` build (no `preserve_order` feature) backs `Value::Object`
//! with `BTreeMap`, so map iteration is key-sorted and serialisation is
//! deterministic for any well-formed `Graph`. If a future crate upgrade
//! changes that, the only observable effect is occasional spurious regens —
//! never a stale build, because the generator output itself stays byte-
//! identical regardless of hash agreement.

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::sync::Arc;

use dashmap::DashMap;

use crate::codegen::{GenerateReport, Generator};
use crate::error::ApiError;
use crate::projects::types::Graph;
use crate::projects::Slug;

/// Outcome of a `regen_if_changed` call. `regenerated` is the contract the
/// caller uses to decide whether to log "regen complete" vs. "skipped";
/// `report` carries the per-file diff when codegen actually ran.
#[derive(Debug)]
pub struct RegenOutcome {
    /// `true` when the generator ran, `false` on a cache hit.
    pub regenerated: bool,
    /// `Some` only when `regenerated == true`. Surfaced by handlers that
    /// want to log the file list; the Build/Run/Test/Debug endpoints
    /// currently do not return it on the wire.
    pub report: Option<GenerateReport>,
}

/// Per-slug "hash of the graph we last regenerated from" cache.
///
/// Cheap to clone — internal state is one `Arc<DashMap<_>>`. Constructed
/// once in `AppState::new` and shared across every handler. Process-local
/// (no on-disk persistence) because the generator itself is idempotent;
/// a server restart just costs one redundant regen on the first click.
#[derive(Clone, Default)]
pub struct CodegenCache {
    last_hash: Arc<DashMap<Slug, u64>>,
}

impl CodegenCache {
    pub fn new() -> Self {
        Self {
            last_hash: Arc::new(DashMap::new()),
        }
    }

    /// Regenerate `slug`'s Rust source from `graph` only if the graph's hash
    /// differs from the last-cached value. On generator failure the hash is
    /// **not** updated — the next click retries, which is the conservative
    /// behaviour (a partial-emit project must be re-emitted to converge).
    ///
    /// Concurrency: two concurrent calls with the same slug+graph may both
    /// pass the hash check and both invoke the generator. The generator's
    /// per-file `write_atomic_if_changed` is safe under concurrent writers
    /// of identical bytes (rename is atomic), so this races to a correct
    /// state — at worst one extra regen runs.
    pub async fn regen_if_changed(
        &self,
        generator: &Generator,
        slug: &Slug,
        graph: &Graph,
    ) -> Result<RegenOutcome, ApiError> {
        let new_hash = hash_graph(graph)?;
        if let Some(existing) = self.last_hash.get(slug) {
            if *existing == new_hash {
                return Ok(RegenOutcome {
                    regenerated: false,
                    report: None,
                });
            }
        }
        let report = generator.generate_project(slug, graph).await?;
        self.last_hash.insert(slug.clone(), new_hash);
        Ok(RegenOutcome {
            regenerated: true,
            report: Some(report),
        })
    }

    /// Test-only: count entries in the cache. Used by adversarial tests to
    /// distinguish "cache stayed empty after a failed regen" from "cache
    /// got populated then ignored."
    #[cfg(test)]
    pub(crate) fn entry_count(&self) -> usize {
        self.last_hash.len()
    }

    /// Drop the cached hash for `slug`. Called on project delete so a
    /// future project recreated with the same slug starts fresh — without
    /// this, a recreated project's first Build click would skip codegen
    /// (the cached hash from the deleted project would still match).
    pub fn forget(&self, slug: &Slug) {
        self.last_hash.remove(slug);
    }
}

/// Hash a graph by serialising to JSON and feeding the bytes to
/// `DefaultHasher`. Errors map to `Internal` — `Graph` is composed of typed
/// fields whose serialisation cannot realistically fail; if it does, the
/// configured serde_json build is broken and an Internal 500 is the right
/// surface.
fn hash_graph(graph: &Graph) -> Result<u64, ApiError> {
    let bytes = serde_json::to_vec(graph)
        .map_err(|e| ApiError::Internal(format!("graph serialise for hash: {e}")))?;
    let mut hasher = DefaultHasher::new();
    hasher.write(&bytes);
    Ok(hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::types::{Node, NodeId, Position, GRAPH_SCHEMA_VERSION};
    use crate::templates::{TemplateId, TemplateRegistry};
    use std::sync::Arc;
    use tempfile::tempdir;

    fn slug(s: &str) -> Slug {
        Slug::new(s).expect("valid slug")
    }

    fn empty_graph() -> Graph {
        Graph::default()
    }

    fn graph_with_dto(name: &str) -> Graph {
        Graph {
            schema_version: GRAPH_SCHEMA_VERSION,
            nodes: vec![Node {
                id: NodeId("n1".into()),
                template_id: TemplateId::new("core.dto").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({"name": name, "fields": [{"name": "id", "ty": "u64"}]}),
                label: None,
            }],
            edges: vec![],
        }
    }

    async fn fresh_gen() -> (tempfile::TempDir, Generator, Slug) {
        let dir = tempdir().unwrap();
        let registry = Arc::new(TemplateRegistry::with_builtins());
        let gen = Generator::new(registry, dir.path().to_path_buf());
        let s = slug("cachetest");
        tokio::fs::create_dir(dir.path().join(s.as_str())).await.unwrap();
        (dir, gen, s)
    }

    #[tokio::test]
    async fn test_first_call_regenerates() {
        let (_dir, gen, s) = fresh_gen().await;
        let cache = CodegenCache::new();
        let out = cache.regen_if_changed(&gen, &s, &empty_graph()).await.unwrap();
        assert!(out.regenerated, "first call must regen");
        assert!(out.report.is_some());
    }

    #[tokio::test]
    async fn test_second_identical_call_is_skipped() {
        let (_dir, gen, s) = fresh_gen().await;
        let cache = CodegenCache::new();
        let g = empty_graph();
        let _ = cache.regen_if_changed(&gen, &s, &g).await.unwrap();
        let out2 = cache.regen_if_changed(&gen, &s, &g).await.unwrap();
        assert!(!out2.regenerated, "identical graph must hit cache");
        assert!(out2.report.is_none());
    }

    #[tokio::test]
    async fn test_modified_graph_regenerates() {
        let (_dir, gen, s) = fresh_gen().await;
        let cache = CodegenCache::new();
        let _ = cache
            .regen_if_changed(&gen, &s, &graph_with_dto("Alpha"))
            .await
            .unwrap();
        let out = cache
            .regen_if_changed(&gen, &s, &graph_with_dto("Beta"))
            .await
            .unwrap();
        assert!(out.regenerated, "different graph must regen");
    }

    #[tokio::test]
    async fn test_revert_to_prior_graph_still_hits_cache_when_hash_matches() {
        // Hash equality, not history, is the cache key. A→B→A reverts to a
        // cache hit on the A click because A's hash is what's stored after
        // the first click... actually no — after B, B's hash is stored, so
        // reverting to A is a *miss*. Pin this exact behaviour.
        let (_dir, gen, s) = fresh_gen().await;
        let cache = CodegenCache::new();
        let a = graph_with_dto("Alpha");
        let b = graph_with_dto("Beta");

        cache.regen_if_changed(&gen, &s, &a).await.unwrap();
        cache.regen_if_changed(&gen, &s, &b).await.unwrap();
        let back_to_a = cache.regen_if_changed(&gen, &s, &a).await.unwrap();
        assert!(back_to_a.regenerated, "A after B is a miss — B is the last cached hash");

        let a_again = cache.regen_if_changed(&gen, &s, &a).await.unwrap();
        assert!(!a_again.regenerated, "A immediately after A is a hit");
    }

    #[tokio::test]
    async fn test_forget_clears_cache_entry() {
        let (_dir, gen, s) = fresh_gen().await;
        let cache = CodegenCache::new();
        let g = empty_graph();
        cache.regen_if_changed(&gen, &s, &g).await.unwrap();
        cache.forget(&s);
        let out = cache.regen_if_changed(&gen, &s, &g).await.unwrap();
        assert!(out.regenerated, "forget must force next call to regen");
    }

    #[tokio::test]
    async fn test_failed_regen_does_not_poison_cache() {
        // Construct a graph whose generator call WILL fail mid-emit, by
        // including a route node connected to a missing handler — actually
        // the generator tolerates that today (skips unconnected routes). A
        // simpler failure mode: invoke with a project dir that does not
        // exist on disk. The generator will fail at `create_dir_all` for a
        // non-writable parent... too OS-dependent.
        //
        // Use a directly-constructed Generator whose projects_root is a
        // path component that exists *as a file* (not a directory), so the
        // generator's first `create_dir_all` fails with NotADirectory.
        let dir = tempdir().unwrap();
        let file_as_root = dir.path().join("not-a-dir");
        tokio::fs::write(&file_as_root, b"x").await.unwrap();
        let registry = Arc::new(TemplateRegistry::with_builtins());
        let gen = Generator::new(registry, file_as_root);
        let s = slug("poison");
        let cache = CodegenCache::new();
        let g = empty_graph();

        let err = cache.regen_if_changed(&gen, &s, &g).await;
        assert!(err.is_err(), "must surface generator failure");

        // Sharper assertion than "second attempt also fails" — directly
        // inspect cache state. After a failed regen, the cache must be
        // empty for this slug, proving the `?` propagation happened
        // BEFORE the `insert`.
        assert_eq!(
            cache.entry_count(),
            0,
            "failed regen must not store hash; cache should be empty"
        );

        let err2 = cache.regen_if_changed(&gen, &s, &g).await;
        assert!(err2.is_err(), "second attempt also fails for the same reason");
        assert_eq!(cache.entry_count(), 0, "still empty after second failure");
    }
}
