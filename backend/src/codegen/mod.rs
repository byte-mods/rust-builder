//! Code generator — turns a user-project's `Graph` into Rust source under
//! `projects/<slug>/src/`.
//!
//! ## Pipeline (per regen call)
//!
//! ```text
//! Graph
//!   │
//!   ├─ for each Node:                                         (T1, this file)
//!   │    template = registry.get(node.template_id)
//!   │    emission = template.emit_runtime(ctx)?               (T5/T6/T7 fill bodies)
//!   │    for each EmittedItem in emission.items:
//!   │       formatted = format::validate_and_format(&item.source)?    (T1)
//!   │       merged    = regions::merge_into_target(target, formatted) (T2)
//!   │       files::write_atomic(target, &merged)?                     (T1)
//!   │
//!   ├─ collect deps from every emission                       (T3)
//!   ├─ cargo_toml::write(deps)                                (T3)
//!   ├─ bootstrap::main_rs() + lib.rs                          (T4)
//!   │
//!   └─ return GenerateReport (touched files, pending templates,
//!      dependency list) — surfaced by `POST /api/projects/:slug/regen`.
//! ```
//!
//! ## Idempotence
//!
//! Re-running on the same graph produces byte-identical output. Two
//! guarantees make this hold:
//! - `prettyplease::unparse` is deterministic (no allocator-order
//!   dependence; stable order of attributes; canonical whitespace).
//! - Regions are spliced into target files keyed by stable IDs derived
//!   from `(template_id, node_id, site)`; the same graph produces the
//!   same set of region keys.
//!
//! ## What this module does NOT do
//!
//! - It does NOT validate the graph against the registry — that already
//!   happened in `ProjectStore::save_graph`. The generator trusts the
//!   on-disk `graph.json` is well-formed at the registry level.
//! - It does NOT run `cargo check` / `cargo build`. Section 6 owns the
//!   build orchestrator and the WebSocket stream of build output. The
//!   generator just produces files; building them is somebody else's job.

pub mod bootstrap;
pub mod cache;
pub mod cargo_toml;
pub mod dataflow;
pub mod files;
pub mod format;
pub mod regions;

pub use cache::{CodegenCache, RegenOutcome};

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::error::ApiError;
use crate::projects::types::Graph;
use crate::projects::Slug;
use crate::templates::TemplateRegistry;

/// Small deserialisable view of `http.route` config — used by the
/// orchestrator to wire routes into `lib.rs` without coupling to the
/// private `builtins` module.
#[derive(Debug, Deserialize)]
struct RouteConfig {
    path: String,
    method: String,
}

/// Small deserialisable view of `http.handler` config — used by the
/// orchestrator to know the handler's function name for route mounting.
#[derive(Debug, Deserialize)]
struct HandlerConfig {
    name: String,
}

/// Reasons code generation can fail. Convertible to `ApiError`.
///
/// `InvalidEmission` is the most important variant — it fires when a
/// template emits source that doesn't parse. Surfacing this to the
/// operator (via the server log) is critical because it means a built-in
/// or user-installed template has a bug.
#[derive(Debug, Error)]
pub enum CodegenError {
    #[error("template `{template_id}` emitted source that does not parse: {error}")]
    InvalidEmission {
        template_id: String,
        node_id: String,
        error: String,
    },

    #[error("template subsystem error: {0}")]
    Template(#[from] crate::templates::TemplateError),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("region merge error: {0}")]
    Region(#[from] regions::RegionError),
}

/// `CodegenError` → `ApiError` mapping policy. `InvalidEmission` and the
/// generic catch-all are server-side bugs (500 `internal`); template-level
/// errors carry through their existing `TemplateError → ApiError` route.
impl From<CodegenError> for ApiError {
    fn from(err: CodegenError) -> Self {
        match err {
            CodegenError::Template(t) => t.into(),
            CodegenError::Io(io) => io.into(),
            CodegenError::InvalidEmission { .. } | CodegenError::Region(_) => {
                ApiError::Internal(err.to_string())
            }
        }
    }
}

/// Outcome of one `generate_project` call. Surfaced verbatim by the
/// `POST /api/projects/:slug/regen` endpoint so the frontend can show the
/// user what changed.
#[derive(Debug, Serialize, Default)]
pub struct GenerateReport {
    /// Relative paths under `projects/<slug>/` that the generator wrote.
    /// Deterministic order (sorted) so two regens on the same graph
    /// produce the same report.
    pub files_written: Vec<String>,

    /// Node-template IDs whose `emit_runtime` returned the
    /// `not_implemented` placeholder. The orchestrator skips writing
    /// these so the user-project does not end up with broken files.
    /// Section 5 / 7 / 9 will gradually empty this set.
    pub pending_templates: Vec<String>,

    /// Aggregated Cargo dependency hints from every emission. Used by T3.
    pub dependencies: Vec<(String, String)>,
}

/// Code generator. Holds the studio's registry + the project root path
/// so `generate_project` knows where to write.
///
/// Cheap to construct per request — the registry is already behind `Arc`
/// from `AppState`.
pub struct Generator {
    registry: Arc<TemplateRegistry>,
    projects_root: PathBuf,
}

impl Generator {
    pub fn new(registry: Arc<TemplateRegistry>, projects_root: PathBuf) -> Self {
        Self { registry, projects_root }
    }

    /// Project's on-disk root: `<projects_root>/<slug>/`.
    pub fn project_dir(&self, slug: &Slug) -> PathBuf {
        self.projects_root.join(slug.as_str())
    }

    /// Generate (or regenerate) the user-project for `slug` from the given
    /// `graph`. Orchestrates bootstrap files, per-node template emissions,
    /// region merging, and Cargo.toml rendering.
    pub async fn generate_project(
        &self,
        slug: &Slug,
        graph: &Graph,
    ) -> Result<GenerateReport, CodegenError> {
        let project_dir = self.project_dir(slug);
        let src_dir = project_dir.join("src");
        tokio::fs::create_dir_all(&src_dir).await?;

        let mut report = GenerateReport::default();
        let mut all_deps: Vec<(String, String)> = Vec::new();
        let dataflow = dataflow::DataflowGraph::analyze(graph, &self.registry);

        // Track module files per directory for mod.rs + lib.rs generation.
        let mut dir_mods: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();

        // Collect spawn expressions for long-running tasks.
        let mut spawn_tasks: Vec<(String, String)> = Vec::new();

        // --- 0. Bootstrap errors.rs (S7) ------------------------------------------
        let errors_src = format::validate_and_format(
            &bootstrap::errors_rs(),
            "bootstrap",
            "errors",
        )?;
        let errors_path = src_dir.join("errors.rs");
        if files::write_atomic_if_changed(&errors_path, &errors_src).await? {
            report.files_written.push("src/errors.rs".to_string());
        }

        // --- 0b. Bootstrap debug.rs (S13) -----------------------------------------
        let debug_src = format::validate_and_format(
            &bootstrap::debug_rs(),
            "bootstrap",
            "debug",
        )?;
        let debug_path = src_dir.join("debug.rs");
        if files::write_atomic_if_changed(&debug_path, &debug_src).await? {
            report.files_written.push("src/debug.rs".to_string());
        }

        // --- 1. Per-node runtime emissions ----------------------------------------
        for node in &graph.nodes {
            let template_id = &node.template_id;
            let Some(template) = self.registry.get(template_id) else {
                continue;
            };

            let ctx = crate::templates::codegen::CodegenCtx {
                project_slug: slug.as_str(),
                node,
                output_root: &project_dir,
                graph,
            };

            let mode = template.codegen_mode();

            // Runtime pass — all modes except pure Codegen.
            if mode != crate::templates::CodegenMode::Codegen {
                let emission = template.emit_runtime(&ctx)?;
                if emission.is_placeholder() {
                    report.pending_templates.push(template_id.as_str().to_string());
                } else {
                    for item in &emission.items {
                        if item.source.contains(".unwrap()") || item.source.contains(".expect(") || item.source.contains("panic!") {
                            return Err(CodegenError::InvalidEmission {
                                template_id: template_id.as_str().to_string(),
                                node_id: node.id.0.clone(),
                                error: "templates are forbidden from emitting unwrap(), expect(), or panic! — use ? propagation or explicit error handling instead".to_string(),
                            });
                        }
                        let replaced_src = dataflow::replace_placeholders(&item.source, &dataflow);
                        let debug_kind = template.debug_bridge();
                        let final_src = if debug_kind != crate::templates::DebugBridgeKind::PassThrough {
                            match instrument_source(&replaced_src, &node.id.0, template_id.as_str()) {
                                Ok(src) => src,
                                Err(e) => {
                                    return Err(CodegenError::InvalidEmission {
                                        template_id: template_id.as_str().to_string(),
                                        node_id: node.id.0.clone(),
                                        error: format!("debug instrumentation failed: {e}"),
                                    });
                                }
                            }
                        } else {
                            replaced_src
                        };
                        let formatted = format::validate_and_format(
                            &final_src,
                            template_id.as_str(),
                            &node.id.0,
                        )?;
                        let target = src_dir.join(&item.module_path);
                        let changed = files::write_atomic_if_changed(&target, &formatted).await?;
                        if changed {
                            report.files_written.push(format!("src/{}", item.module_path));
                        }
                        if let Some((dir, file)) = item.module_path.rsplit_once('/') {
                            let name = file.trim_end_matches(".rs");
                            dir_mods.entry(dir.to_string()).or_default().push(name.to_string());
                        }
                    }
                    if template.debug_bridge() == crate::templates::DebugBridgeKind::LongRunner {
                        for item in &emission.items {
                            if let Some((dir, file)) = item.module_path.rsplit_once('/') {
                                let name = file.trim_end_matches(".rs");
                                let crate_name = slug.as_str().replace('-', "_");
                                let expr = format!("{}::{}::{}::run()", crate_name, dir.replace('/', "::"), name);
                                spawn_tasks.push((name.to_string(), expr));
                            }
                        }
                    }
                    all_deps.extend(emission.dependencies);
                }
            }

            // Schema pass — Codegen and Both modes.
            if mode != crate::templates::CodegenMode::Runtime {
                let schema_emission = template.emit_schema(&ctx)?;
                if !schema_emission.is_placeholder() {
                    for item in &schema_emission.items {
                        if item.source.contains(".unwrap()") || item.source.contains(".expect(") || item.source.contains("panic!") {
                            return Err(CodegenError::InvalidEmission {
                                template_id: template_id.as_str().to_string(),
                                node_id: node.id.0.clone(),
                                error: "templates are forbidden from emitting unwrap(), expect(), or panic! — use ? propagation or explicit error handling instead".to_string(),
                            });
                        }
                        let replaced_src = dataflow::replace_placeholders(&item.source, &dataflow);
                        let formatted = format::validate_and_format(
                            &replaced_src,
                            template_id.as_str(),
                            &node.id.0,
                        )?;
                        let target = src_dir.join(&item.module_path);
                        let changed = files::write_atomic_if_changed(&target, &formatted).await?;
                        if changed {
                            report.files_written.push(format!("src/{}", item.module_path));
                        }
                        if let Some((dir, file)) = item.module_path.rsplit_once('/') {
                            let name = file.trim_end_matches(".rs");
                            dir_mods.entry(dir.to_string()).or_default().push(name.to_string());
                        }
                    }
                    all_deps.extend(schema_emission.dependencies);
                }
            }
        }

        // --- 2. main.rs (fully regenerated each pass) -----------------------------
        let main_src = bootstrap::main_rs(slug, &spawn_tasks);
        let main_path = src_dir.join("main.rs");
        if files::write_atomic_if_changed(&main_path, &main_src).await? {
            report.files_written.push("src/main.rs".to_string());
        }

        // --- 3. lib.rs with @generated regions ------------------------------------
        let lib_base = if src_dir.join("lib.rs").exists() {
            tokio::fs::read_to_string(src_dir.join("lib.rs")).await?
        } else {
            bootstrap::lib_rs(slug)
        };

        let mut lib_regions = std::collections::HashMap::new();

        let mut module_decls = String::new();
        module_decls.push_str("pub mod errors;\n");
        module_decls.push_str("pub mod debug;\n");
        for dir in dir_mods.keys() {
            module_decls.push_str(&format!("pub mod {};\n", dir));
        }
        lib_regions.insert("module_decls".to_string(), module_decls);

        let mut routes = String::new();
        for node in &graph.nodes {
            if node.template_id.as_str() != "http.route" {
                continue;
            }
            let route_cfg: RouteConfig = serde_json::from_value(node.config.clone())
                .map_err(|e| CodegenError::InvalidEmission {
                    template_id: "http.route".to_string(),
                    node_id: node.id.0.clone(),
                    error: e.to_string(),
                })?;

            // Find the handler connected to this route's output port.
            let handler_id = graph.edges.iter()
                .find(|e| e.source == node.id)
                .map(|e| &e.target);

            if let Some(handler_id) = handler_id {
                if let Some(handler_node) = graph.nodes.iter().find(|n| &n.id == handler_id) {
                    let handler_cfg: HandlerConfig = serde_json::from_value(handler_node.config.clone())
                        .map_err(|e| CodegenError::InvalidEmission {
                            template_id: "http.handler".to_string(),
                            node_id: handler_node.id.0.clone(),
                            error: e.to_string(),
                        })?;

                    let method_fn = match route_cfg.method.as_str() {
                        "GET" => "get",
                        "POST" => "post",
                        "PUT" => "put",
                        "DELETE" => "delete",
                        _ => "get",
                    };

                    routes.push_str(&format!(
                        "    r = r.route(\"{}\", axum::routing::{}(handlers::{}::{}));\n",
                        route_cfg.path, method_fn, handler_cfg.name, handler_cfg.name
                    ));
                }
            }
        }
        if !routes.is_empty() {
            lib_regions.insert("routes".to_string(), routes);
        }

        let lib_merged = if lib_regions.is_empty() {
            lib_base
        } else {
            regions::merge(&lib_base, &lib_regions).map_err(CodegenError::Region)?.text
        };
        let lib_path = src_dir.join("lib.rs");
        if files::write_atomic_if_changed(&lib_path, &lib_merged).await? {
            report.files_written.push("src/lib.rs".to_string());
        }

        // --- 4. Subdirectory mod.rs files -----------------------------------------
        for (dir, mods) in &dir_mods {
            if mods.is_empty() {
                continue;
            }
            let dir_path = src_dir.join(dir);
            tokio::fs::create_dir_all(&dir_path).await?;

            let mod_path = dir_path.join("mod.rs");
            let mod_base = if mod_path.exists() {
                tokio::fs::read_to_string(&mod_path).await?
            } else {
                "// @generated:begin mods\n// @generated:end mods\n".to_string()
            };

            let mut mod_body = String::new();
            for name in mods {
                mod_body.push_str(&format!("pub mod {};\n", name));
            }
            let mod_regions = std::collections::HashMap::from([("mods".to_string(), mod_body)]);
            let mod_merged = regions::merge(&mod_base, &mod_regions).map_err(CodegenError::Region)?.text;

            if files::write_atomic_if_changed(&mod_path, &mod_merged).await? {
                report.files_written.push(format!("src/{}/mod.rs", dir));
            }
        }

        // --- 5. Cargo.toml --------------------------------------------------------
        all_deps.sort_by(|a, b| a.0.cmp(&b.0));
        all_deps.dedup_by(|a, b| a.0 == b.0);
        let cargo_src = cargo_toml::render(slug, &all_deps);
        let cargo_path = project_dir.join("Cargo.toml");
        if files::write_atomic_if_changed(&cargo_path, &cargo_src).await? {
            report.files_written.push("Cargo.toml".to_string());
        }
        report.dependencies = all_deps;

        // --- 6. CLAUDE.md ---------------------------------------------------------
        let claude_src = bootstrap::claude_md(slug);
        let claude_path = project_dir.join("CLAUDE.md");
        if files::write_atomic_if_changed(&claude_path, &claude_src).await? {
            report.files_written.push("CLAUDE.md".to_string());
        }

        report.files_written.sort();
        report.files_written.dedup();
        report.pending_templates.sort();
        report.pending_templates.dedup();

        Ok(report)
    }
}

/// Parse, traverse, and wrap functions inside generated source files with visual debugger hooks.
fn instrument_source(
    source: &str,
    node_id: &str,
    template_id: &str,
) -> Result<String, ::syn::Error> {
    let mut file: ::syn::File = ::syn::parse_file(source)?;
    let mut changed = false;

    for item in &mut file.items {
        if let ::syn::Item::Fn(item_fn) = item {
            // Collect inputs to the function
            let mut arg_idents = Vec::new();
            for input in &item_fn.sig.inputs {
                if let ::syn::FnArg::Typed(pat_type) = input {
                    if let ::syn::Pat::Ident(pat_ident) = &*pat_type.pat {
                        arg_idents.push(pat_ident.ident.clone());
                    }
                }
            }

            let inputs_expr: ::syn::Expr = if arg_idents.is_empty() {
                ::syn::parse_quote!(())
            } else if arg_idents.len() == 1 {
                let first = &arg_idents[0];
                ::syn::parse_quote!(&#first)
            } else {
                ::syn::parse_quote!((#(&#arg_idents),*))
            };

            let stmts = &item_fn.block.stmts;
            let node_id_str = node_id.to_string();

            let new_block: ::syn::Block = if item_fn.sig.asyncness.is_some() {
                let after_hook: ::syn::Stmt = if template_id == "http.handler" {
                    ::syn::parse_quote! {
                        let debug_res = match &result {
                            Ok(_) => Ok("IntoResponse"),
                            Err(e) => Err(e),
                        };
                    }
                } else {
                    ::syn::parse_quote! {
                        let debug_res = &result;
                    }
                };
                
                let after_call: ::syn::Stmt = if template_id == "http.handler" {
                    ::syn::parse_quote! {
                        crate::debug::bridge_after(#node_id_str, &debug_res);
                    }
                } else {
                    ::syn::parse_quote! {
                        crate::debug::bridge_after(#node_id_str, debug_res);
                    }
                };

                ::syn::parse_quote!({
                    crate::debug::bridge_before(#node_id_str, &#inputs_expr);
                    let result = (|| async {
                        #(#stmts)*
                    })().await;
                    #after_hook
                    #after_call
                    result
                })
            } else {
                ::syn::parse_quote!({
                    crate::debug::bridge_before(#node_id_str, &#inputs_expr);
                    let result = (|| {
                        #(#stmts)*
                    })();
                    crate::debug::bridge_after(#node_id_str, &result);
                    result
                })
            };

            item_fn.block = Box::new(new_block);
            changed = true;
        }
    }

    if changed {
        Ok(::prettyplease::unparse(&file))
    } else {
        Ok(source.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use crate::templates::{NodeTemplate, TemplateId, TemplateDisplay, PortSpec};

    #[tokio::test]
    async fn test_generate_project_creates_src_dir() {
        let dir = tempdir().unwrap();
        let registry = Arc::new(TemplateRegistry::with_builtins());
        let gen = Generator::new(registry, dir.path().to_path_buf());
        let slug = Slug::new("smoke").unwrap();
        let graph = Graph::default();

        // Project dir is created on demand.
        tokio::fs::create_dir(dir.path().join("smoke")).await.unwrap();
        let report = gen.generate_project(&slug, &graph).await.unwrap();
        assert!(dir.path().join("smoke/src").is_dir());
        assert!(
            report.files_written.contains(&"src/main.rs".to_string()),
            "main.rs must be written even for empty graph"
        );
        assert!(
            report.files_written.contains(&"src/errors.rs".to_string()),
            "errors.rs must be written even for empty graph"
        );
    }

    #[tokio::test]
    async fn test_generate_project_emits_dto_service_handler_and_routes() {
        let dir = tempdir().unwrap();
        let registry = Arc::new(TemplateRegistry::with_builtins());
        let gen = Generator::new(registry, dir.path().to_path_buf());
        let slug = Slug::new("demo").unwrap();

        tokio::fs::create_dir(dir.path().join("demo")).await.unwrap();

        let graph = Graph {
            schema_version: crate::projects::types::GRAPH_SCHEMA_VERSION,
            nodes: vec![
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("n1".into()),
                    template_id: crate::templates::TemplateId::new("core.dto").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"name": "User", "fields": [{"name": "id", "ty": "u64"}]}),
                    label: None,
                },
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("n2".into()),
                    template_id: crate::templates::TemplateId::new("core.service").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"name": "get_user"}),
                    label: None,
                },
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("n3".into()),
                    template_id: crate::templates::TemplateId::new("http.handler").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"name": "hello"}),
                    label: None,
                },
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("n4".into()),
                    template_id: crate::templates::TemplateId::new("http.route").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"path": "/hello", "method": "GET"}),
                    label: None,
                },
            ],
            edges: vec![
                crate::projects::types::Edge {
                    id: crate::projects::types::EdgeId("e1".into()),
                    source: crate::projects::types::NodeId("n4".into()),
                    target: crate::projects::types::NodeId("n3".into()),
                    source_port: "request".to_string(),
                    target_port: "request".to_string(),
                },
            ],
        };

        let report = gen.generate_project(&slug, &graph).await.unwrap();

        // All expected files were written.
        assert!(
            report.files_written.contains(&"src/dto/user.rs".to_string()),
            "dto file missing: {:?}",
            report.files_written
        );
        assert!(report.files_written.contains(&"src/services/get_user.rs".to_string()));
        assert!(report.files_written.contains(&"src/handlers/hello.rs".to_string()));
        assert!(report.files_written.contains(&"src/errors.rs".to_string()));
        assert!(report.files_written.contains(&"src/lib.rs".to_string()));
        assert!(report.files_written.contains(&"Cargo.toml".to_string()));

        // lib.rs should contain the route mount and errors module.
        let lib_src = tokio::fs::read_to_string(dir.path().join("demo/src/lib.rs")).await.unwrap();
        assert!(lib_src.contains(".route(\"/hello\""), "lib.rs must mount route: {lib_src}");
        assert!(lib_src.contains("handlers::hello::hello"));
        assert!(lib_src.contains("mod errors;"), "lib.rs must declare errors module: {lib_src}");

        // dto file should parse as Rust.
        let dto_src = tokio::fs::read_to_string(dir.path().join("demo/src/dto/user.rs")).await.unwrap();
        assert!(syn::parse_file(&dto_src).is_ok(), "dto must be valid Rust");
    }

    #[tokio::test]
    async fn test_generate_project_spawns_long_runners_in_main_rs() {
        let dir = tempdir().unwrap();
        let registry = Arc::new(TemplateRegistry::with_builtins());
        let gen = Generator::new(registry, dir.path().to_path_buf());
        let slug = Slug::new("lr").unwrap();

        tokio::fs::create_dir(dir.path().join("lr")).await.unwrap();

        let graph = Graph {
            schema_version: crate::projects::types::GRAPH_SCHEMA_VERSION,
            nodes: vec![
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("n1".into()),
                    template_id: crate::templates::TemplateId::new("integration.consumer.placeholder").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"topic": "orders", "group": "g1", "name": "orders_consumer"}),
                    label: None,
                },
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("n2".into()),
                    template_id: crate::templates::TemplateId::new("integration.scheduler.placeholder").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"cron": "0 9 * * *", "name": "morning"}),
                    label: None,
                },
            ],
            edges: vec![],
        };

        let report = gen.generate_project(&slug, &graph).await.unwrap();

        assert!(
            report.files_written.contains(&"src/consumers/orders_consumer.rs".to_string()),
            "consumer file missing: {:?}",
            report.files_written
        );
        assert!(report.files_written.contains(&"src/schedulers/morning.rs".to_string()));
        assert!(report.files_written.contains(&"src/main.rs".to_string()));
        assert!(report.files_written.contains(&"src/lib.rs".to_string()));

        let main_src = tokio::fs::read_to_string(dir.path().join("lr/src/main.rs")).await.unwrap();
        assert!(main_src.contains("async fn supervise"), "main.rs must contain supervise");
        assert!(main_src.contains("tokio::spawn(supervise(\"orders_consumer\""));
        assert!(main_src.contains("lr::consumers::orders_consumer::run()"), "main_src did not contain expected orders consumer, content:\n{}", main_src);
        assert!(main_src.contains("tokio::spawn(supervise(\"morning\""));
        assert!(main_src.contains("lr::schedulers::morning::run()"), "main_src did not contain expected morning run, content:\n{}", main_src);

        let lib_src = tokio::fs::read_to_string(dir.path().join("lr/src/lib.rs")).await.unwrap();
        assert!(lib_src.contains("mod consumers;"), "lib.rs must declare consumers: {lib_src}");
        assert!(lib_src.contains("mod schedulers;"));

        assert!(syn::parse_file(&main_src).is_ok(), "main.rs must be valid Rust");
    }

    #[tokio::test]
    async fn test_generate_project_wires_handler_to_service_and_service_to_dto() {
        let dir = tempdir().unwrap();
        let registry = Arc::new(TemplateRegistry::with_builtins());
        let gen = Generator::new(registry, dir.path().to_path_buf());
        let slug = Slug::new("wired").unwrap();

        tokio::fs::create_dir(dir.path().join("wired")).await.unwrap();

        let graph = Graph {
            schema_version: crate::projects::types::GRAPH_SCHEMA_VERSION,
            nodes: vec![
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("n1".into()),
                    template_id: crate::templates::TemplateId::new("core.dto").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"name": "User", "fields": [{"name": "id", "ty": "u64"}]}),
                    label: None,
                },
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("n2".into()),
                    template_id: crate::templates::TemplateId::new("core.service").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"name": "get_user"}),
                    label: None,
                },
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("n3".into()),
                    template_id: crate::templates::TemplateId::new("http.handler").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"name": "hello"}),
                    label: None,
                },
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("n4".into()),
                    template_id: crate::templates::TemplateId::new("http.route").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"path": "/hello", "method": "GET"}),
                    label: None,
                },
            ],
            edges: vec![
                // DTO → Service
                crate::projects::types::Edge {
                    id: crate::projects::types::EdgeId("e1".into()),
                    source: crate::projects::types::NodeId("n1".into()),
                    target: crate::projects::types::NodeId("n2".into()),
                    source_port: "output".to_string(),
                    target_port: "input".to_string(),
                },
                // Service → Handler
                crate::projects::types::Edge {
                    id: crate::projects::types::EdgeId("e2".into()),
                    source: crate::projects::types::NodeId("n2".into()),
                    target: crate::projects::types::NodeId("n3".into()),
                    source_port: "output".to_string(),
                    target_port: "request".to_string(),
                },
                // Route → Handler
                crate::projects::types::Edge {
                    id: crate::projects::types::EdgeId("e3".into()),
                    source: crate::projects::types::NodeId("n4".into()),
                    target: crate::projects::types::NodeId("n3".into()),
                    source_port: "request".to_string(),
                    target_port: "request".to_string(),
                },
            ],
        };

        let report = gen.generate_project(&slug, &graph).await.unwrap();

        // All expected files were written.
        assert!(report.files_written.contains(&"src/dto/user.rs".to_string()), "dto missing: {:?}", report.files_written);
        assert!(report.files_written.contains(&"src/services/get_user.rs".to_string()));
        assert!(report.files_written.contains(&"src/handlers/hello.rs".to_string()));
        assert!(report.files_written.contains(&"src/lib.rs".to_string()));

        // Service should import DTO.
        let svc_src = tokio::fs::read_to_string(dir.path().join("wired/src/services/get_user.rs")).await.unwrap();
        assert!(svc_src.contains("use crate::dto::user::User;"), "service must import DTO: {svc_src}");
        assert!(svc_src.contains("let _: Option<User> = None;"));
        assert!(syn::parse_file(&svc_src).is_ok(), "service must be valid Rust");

        // Handler should import and call service.
        let handler_src = tokio::fs::read_to_string(dir.path().join("wired/src/handlers/hello.rs")).await.unwrap();
        assert!(handler_src.contains("use crate::services::get_user;"), "handler must import service: {handler_src}");
        assert!(handler_src.contains("let _ = get_user::get_user().await?;"));
        assert!(syn::parse_file(&handler_src).is_ok(), "handler must be valid Rust");

        // lib.rs should still mount the route.
        let lib_src = tokio::fs::read_to_string(dir.path().join("wired/src/lib.rs")).await.unwrap();
        assert!(lib_src.contains(".route(\"/hello\""));
    }

    #[test]
    fn test_invalid_emission_maps_to_internal_500() {
        let err = CodegenError::InvalidEmission {
            template_id: "foo.bar".into(),
            node_id: "n1".into(),
            error: "unexpected token".into(),
        };
        let api: ApiError = err.into();
        assert!(matches!(api, ApiError::Internal(_)));
    }

    #[tokio::test]
    async fn test_generate_project_rejects_unwrap_or_expect() {
        let dir = tempdir().unwrap();
        
        struct PanicTemplate;
        impl NodeTemplate for PanicTemplate {
            fn id(&self) -> &TemplateId {
                static ID: std::sync::OnceLock<TemplateId> = std::sync::OnceLock::new();
                ID.get_or_init(|| TemplateId::new("test.panic").unwrap())
            }
            fn display(&self) -> &TemplateDisplay {
                static DISPLAY: std::sync::OnceLock<TemplateDisplay> = std::sync::OnceLock::new();
                DISPLAY.get_or_init(|| TemplateDisplay::new("Panic", "test", "panic template"))
            }
            fn input_ports(&self) -> &[PortSpec] { &[] }
            fn output_ports(&self) -> &[PortSpec] { &[] }
            fn config_schema(&self) -> &serde_json::Value {
                static SCHEMA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
                SCHEMA.get_or_init(|| serde_json::json!({"type": "object"}))
            }
            fn emit_runtime(
                &self,
                _ctx: &crate::templates::codegen::CodegenCtx<'_>,
            ) -> Result<crate::templates::codegen::RuntimeEmission, crate::templates::TemplateError> {
                Ok(crate::templates::codegen::RuntimeEmission {
                    items: vec![crate::templates::codegen::EmittedItem {
                        module_path: "panic.rs".to_string(),
                        source: "pub fn do_panic() { let x: Option<i32> = None; x.unwrap(); }".to_string(),
                    }],
                    dependencies: vec![],
                    debug_site: None,
                })
            }
        }

        let mut registry = TemplateRegistry::new();
        registry.insert(Arc::new(PanicTemplate));
        let gen = Generator::new(Arc::new(registry), dir.path().to_path_buf());
        let slug = Slug::new("unwrap-test").unwrap();
        tokio::fs::create_dir(dir.path().join("unwrap-test")).await.unwrap();

        let graph = Graph {
            schema_version: crate::projects::types::GRAPH_SCHEMA_VERSION,
            nodes: vec![
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("n1".into()),
                    template_id: TemplateId::new("test.panic").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({}),
                    label: None,
                },
            ],
            edges: vec![],
        };

        let err = gen.generate_project(&slug, &graph).await.unwrap_err();
        assert!(
            err.to_string().contains("forbidden from emitting unwrap()"),
            "expected unwrap rejection error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_generate_project_error_signatures_compile() {
        let dir = tempdir().unwrap();
        let registry = Arc::new(TemplateRegistry::with_builtins());
        let gen = Generator::new(registry, dir.path().to_path_buf());
        let slug = Slug::new("error-test").unwrap();

        tokio::fs::create_dir(dir.path().join("error-test")).await.unwrap();

        let graph = crate::projects::types::Graph {
            schema_version: crate::projects::types::GRAPH_SCHEMA_VERSION,
            nodes: vec![
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("n1".into()),
                    template_id: crate::templates::TemplateId::new("core.dto").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"name": "User", "fields": [{"name": "id", "ty": "u64"}]}),
                    label: None,
                },
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("n2".into()),
                    template_id: crate::templates::TemplateId::new("core.service").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"name": "get_user"}),
                    label: None,
                },
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("n3".into()),
                    template_id: crate::templates::TemplateId::new("http.handler").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"name": "hello"}),
                    label: None,
                },
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("n4".into()),
                    template_id: crate::templates::TemplateId::new("http.route").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"path": "/hello", "method": "GET"}),
                    label: None,
                },
            ],
            edges: vec![
                crate::projects::types::Edge {
                    id: crate::projects::types::EdgeId("e1".into()),
                    source: crate::projects::types::NodeId("n1".into()),
                    target: crate::projects::types::NodeId("n2".into()),
                    source_port: "output".into(),
                    target_port: "input".into(),
                },
                crate::projects::types::Edge {
                    id: crate::projects::types::EdgeId("e2".into()),
                    source: crate::projects::types::NodeId("n2".into()),
                    target: crate::projects::types::NodeId("n3".into()),
                    source_port: "output".into(),
                    target_port: "request".into(),
                },
                crate::projects::types::Edge {
                    id: crate::projects::types::EdgeId("e3".into()),
                    source: crate::projects::types::NodeId("n4".into()),
                    target: crate::projects::types::NodeId("n3".into()),
                    source_port: "request".into(),
                    target_port: "request".into(),
                },
            ],
        };

        let report = gen.generate_project(&slug, &graph).await.unwrap();

        assert!(report.files_written.contains(&"src/errors.rs".to_string()));
        assert!(report.files_written.contains(&"src/services/get_user.rs".to_string()));
        assert!(report.files_written.contains(&"src/handlers/hello.rs".to_string()));
        assert!(report.files_written.contains(&"src/lib.rs".to_string()));

        // errors.rs parses and contains AppError + IntoResponse
        let errors_src = tokio::fs::read_to_string(dir.path().join("error-test/src/errors.rs")).await.unwrap();
        assert!(syn::parse_file(&errors_src).is_ok(), "errors.rs must be valid Rust");
        assert!(errors_src.contains("pub enum AppError"));
        assert!(errors_src.contains("impl IntoResponse for AppError"));

        // service returns Result with AppError
        let svc_src = tokio::fs::read_to_string(dir.path().join("error-test/src/services/get_user.rs")).await.unwrap();
        assert!(svc_src.contains("Result<&'static str, AppError>"), "service must return Result: {svc_src}");
        assert!(syn::parse_file(&svc_src).is_ok(), "service must be valid Rust");

        // handler returns Result and propagates with ?
        let handler_src = tokio::fs::read_to_string(dir.path().join("error-test/src/handlers/hello.rs")).await.unwrap();
        assert!(handler_src.contains("Result<impl IntoResponse, AppError>"), "handler must return Result: {handler_src}");
        assert!(handler_src.contains("get_user::get_user().await?"), "handler must propagate with ?: {handler_src}");
        assert!(syn::parse_file(&handler_src).is_ok(), "handler must be valid Rust");

        // lib.rs declares errors module and mounts route
        let lib_src = tokio::fs::read_to_string(dir.path().join("error-test/src/lib.rs")).await.unwrap();
        assert!(lib_src.contains("mod errors;"), "lib.rs must declare errors: {lib_src}");
        assert!(lib_src.contains(".route(\"/hello\""), "lib.rs must mount route: {lib_src}");
    }

    #[tokio::test]
    async fn test_generate_project_parsers_compile() {
        let dir = tempdir().unwrap();
        let registry = Arc::new(TemplateRegistry::with_builtins());
        let gen = Generator::new(registry, dir.path().to_path_buf());
        let slug = Slug::new("parser-test").unwrap();

        let project_dir = dir.path().join("parser-test");
        tokio::fs::create_dir(&project_dir).await.unwrap();

        // Write schema files into the project directory.
        tokio::fs::write(
            project_dir.join("person.json"),
            r#"{"title":"Person","type":"object","properties":{"name":{"type":"string"},"age":{"type":"integer"}},"required":["name","age"]}"#,
        ).await.unwrap();
        tokio::fs::write(
            project_dir.join("person.proto"),
            r#"syntax = "proto3";
package person;
message Person {
  string name = 1;
  int32 age = 2;
}
"#,
        ).await.unwrap();

        let graph = crate::projects::types::Graph {
            schema_version: crate::projects::types::GRAPH_SCHEMA_VERSION,
            nodes: vec![
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("p1".into()),
                    template_id: crate::templates::TemplateId::new("parser.json").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"schema_file": "person.json", "name": "person_json"}),
                    label: None,
                },
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("p2".into()),
                    template_id: crate::templates::TemplateId::new("parser.protobuf").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"schema_file": "person.proto", "name": "person_proto"}),
                    label: None,
                },
            ],
            edges: vec![],
        };

        let report = gen.generate_project(&slug, &graph).await.unwrap();

        assert!(
            report.files_written.contains(&"src/parsers/person_json.rs".to_string()),
            "json parser file missing: {:?}",
            report.files_written
        );
        assert!(
            report.files_written.contains(&"src/parsers/person_proto.rs".to_string()),
            "protobuf parser file missing: {:?}",
            report.files_written
        );
        assert!(
            report.dependencies.contains(&("prost".to_string(), "0.14".to_string())),
            "prost dep missing: {:?}",
            report.dependencies
        );

        let json_src = tokio::fs::read_to_string(project_dir.join("src/parsers/person_json.rs")).await.unwrap();
        assert!(json_src.contains("struct Person"), "json parser must contain Person: {json_src}");
        assert!(syn::parse_file(&json_src).is_ok(), "json parser must be valid Rust");

        let proto_src = tokio::fs::read_to_string(project_dir.join("src/parsers/person_proto.rs")).await.unwrap();
        assert!(proto_src.contains("struct Person"), "proto parser must contain Person: {proto_src}");
        assert!(proto_src.contains("::prost::Message"), "proto parser must use prost::Message: {proto_src}");
        assert!(syn::parse_file(&proto_src).is_ok(), "proto parser must be valid Rust");

        let lib_src = tokio::fs::read_to_string(project_dir.join("src/lib.rs")).await.unwrap();
        assert!(lib_src.contains("mod parsers;"), "lib.rs must declare parsers: {lib_src}");
    }

    #[tokio::test]
    async fn test_generate_project_is_idempotent_with_parsers() {
        let dir = tempdir().unwrap();
        let registry = Arc::new(TemplateRegistry::with_builtins());
        let gen = Generator::new(registry, dir.path().to_path_buf());
        let slug = Slug::new("idempotent").unwrap();

        let project_dir = dir.path().join("idempotent");
        tokio::fs::create_dir(&project_dir).await.unwrap();
        tokio::fs::write(
            project_dir.join("person.json"),
            r#"{"title":"Person","type":"object","properties":{"name":{"type":"string"}},"required":["name"]}"#,
        ).await.unwrap();
        tokio::fs::write(
            project_dir.join("person.proto"),
            r#"syntax = "proto3"; package person; message Person { string name = 1; }"#,
        ).await.unwrap();

        let graph = crate::projects::types::Graph {
            schema_version: crate::projects::types::GRAPH_SCHEMA_VERSION,
            nodes: vec![
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("p1".into()),
                    template_id: crate::templates::TemplateId::new("parser.json").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"schema_file": "person.json", "name": "person_json"}),
                    label: None,
                },
                crate::projects::types::Node {
                    id: crate::projects::types::NodeId("p2".into()),
                    template_id: crate::templates::TemplateId::new("parser.protobuf").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"schema_file": "person.proto", "name": "person_proto"}),
                    label: None,
                },
            ],
            edges: vec![],
        };

        let report1 = gen.generate_project(&slug, &graph).await.unwrap();
        let report2 = gen.generate_project(&slug, &graph).await.unwrap();

        // Second run should report no files changed (idempotency).
        assert!(
            report2.files_written.is_empty(),
            "second run must report no changes: {:?}",
            report2.files_written
        );
        assert_eq!(report1.dependencies, report2.dependencies, "dependencies must be identical across runs");

        // All files from the first run must still exist with identical content.
        for file in &report1.files_written {
            let path = project_dir.join(file);
            let content_after_first = tokio::fs::read_to_string(&path).await.unwrap();
            let content_after_second = tokio::fs::read_to_string(&path).await.unwrap();
            assert_eq!(
                content_after_first, content_after_second,
                "{} must be byte-identical across runs",
                file
            );
        }
    }
}
