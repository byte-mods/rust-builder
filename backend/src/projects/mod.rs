//! Project domain — the persisted model for a user-project (the metadata
//! header `project.json` and the flow graph `graph.json`), the validated
//! `Slug` identifier, and the filesystem-backed `ProjectStore` that drives
//! CRUD.
//!
//! The HTTP surface for these types lives in `crate::projects::handlers`;
//! that module is wired into `crate::router()` under `/api/projects`.

pub mod handlers;
pub mod store;
pub mod types;
pub mod validation;
pub mod llm;
pub mod security;
pub mod db;
pub mod collab;

pub use collab::{collab_ws, CollabManager};

pub use handlers::projects_router;
pub use store::ProjectStore;
pub use types::{
    Edge, EdgeId, Graph, Node, NodeId, NodeKind, Package, PackageId, PackageSlug,
    PackageTreeError, Position, Project, ProjectMeta, Slug, SlugError, GRAPH_SCHEMA_VERSION,
    PROJECT_SCHEMA_VERSION,
};
