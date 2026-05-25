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
    Graph, Package, PackageId, PackageSlug, Project, ProjectMeta, Slug, GRAPH_SCHEMA_VERSION,
    PROJECT_SCHEMA_VERSION,
};

const PROJECT_FILE: &str = "project.json";
/// Legacy single-graph filename. Still recognised on load for one-shot
/// migration; never written by current code after T2.
const GRAPH_FILE: &str = "graph.json";
/// Directory under each project that contains one folder per package,
/// each folder owning its own `graph.json`. Created on first save (or
/// during legacy migration if a pre-Section-1 project is opened).
const PACKAGES_DIR: &str = "packages";
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

    /// Directory holding all package folders for `slug`. Created lazily by
    /// the migration helper or by `create_project`.
    fn packages_dir(&self, slug: &Slug) -> PathBuf {
        self.project_dir(slug).join(PACKAGES_DIR)
    }

    /// Directory for a single package within a project.
    /// `projects/<slug>/packages/<pkg-slug>/`. Both slug types are
    /// validated FS-safe — defence in depth.
    fn package_dir(&self, slug: &Slug, pkg_slug: &PackageSlug) -> PathBuf {
        self.packages_dir(slug).join(pkg_slug.as_str())
    }

    /// Full path to a package's `graph.json` file.
    fn package_graph_path(&self, slug: &Slug, pkg_slug: &PackageSlug) -> PathBuf {
        self.package_dir(slug, pkg_slug).join(GRAPH_FILE)
    }

    /// One-shot migration from the pre-Section-1 layout to the
    /// multi-package layout. Idempotent: safe to call on every load and
    /// every save; the no-op cost is two `fs::metadata` calls.
    ///
    /// Behaviour:
    /// - If `packages/main/graph.json` exists → no-op (migration already
    ///   done, or the project was created post-Section-1).
    /// - If only the legacy `graph.json` exists at the project root →
    ///   create `packages/main/` and atomically rename the file in.
    /// - If neither exists → no-op (the project has no graph yet; e.g.
    ///   freshly created project before any save).
    /// - If both legacy `graph.json` AND `packages/main/graph.json`
    ///   exist → log a warning and leave the new location authoritative.
    ///   This signals a previous crash mid-migration; the operator can
    ///   reconcile manually.
    async fn migrate_legacy_graph_if_needed(&self, slug: &Slug) -> Result<(), ApiError> {
        let dir = self.project_dir(slug);
        let new_path = self.package_graph_path(slug, &PackageSlug::root());
        let legacy_path = dir.join(GRAPH_FILE);

        let new_exists = fs::metadata(&new_path).await.is_ok();
        let legacy_exists = fs::metadata(&legacy_path).await.is_ok();

        match (new_exists, legacy_exists) {
            (true, false) | (false, false) => Ok(()),
            (true, true) => {
                warn!(
                    slug = %slug,
                    "both legacy graph.json and packages/main/graph.json exist; \
                     leaving new location authoritative — operator should remove \
                     the legacy file once verified"
                );
                Ok(())
            }
            (false, true) => {
                // Ensure packages/main/ exists, then rename. `create_dir_all`
                // is idempotent so a partial prior run (dir created but
                // rename never happened) re-converges here.
                //
                // Concurrent-first-load race: two callers can both observe
                // `(new=false, legacy=true)` before either rename completes,
                // because reads (load / load_graph / export_archive) do not
                // acquire the per-slug write lock. The loser of the rename
                // race sees `NotFound` because the winner already moved the
                // file. Treat that as "migration already done by someone
                // else" rather than a hard failure — the post-condition
                // (graph is at the new path) holds either way. Any other
                // error propagates normally.
                let root_pkg_dir = self.package_dir(slug, &PackageSlug::root());
                fs::create_dir_all(&root_pkg_dir).await?;
                match fs::rename(&legacy_path, &new_path).await {
                    Ok(()) => {
                        tracing::info!(
                            slug = %slug,
                            "migrated legacy graph.json → packages/main/graph.json"
                        );
                        Ok(())
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        // Another concurrent load completed the migration
                        // between our metadata probe and the rename call.
                        // Acceptable — the desired end state is reached.
                        tracing::debug!(
                            slug = %slug,
                            "legacy graph.json already moved by a concurrent loader"
                        );
                        Ok(())
                    }
                    Err(err) => Err(err.into()),
                }
            }
        }
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

    /// Private helper to seed a template graph and return it along with pre-installed marketplace packages
    fn seed_template_graph(&self, template: &str) -> (Graph, Vec<String>) {
        use crate::projects::types::{Node, Edge, NodeId, EdgeId, Position};
        use crate::templates::TemplateId;

        let mut graph = Graph::default();
        let mut packages = Vec::new();

        match template {
            "ecommerce" => {
                packages.push("mongodb".to_string());
                packages.push("redis".to_string());

                // 1. Entry Point
                graph.nodes.push(Node {
                    id: NodeId("entry".into()),
                    template_id: TemplateId::new("core.entry_point").unwrap(),
                    position: Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({
                        "bind_address": "127.0.0.1:8080",
                        "log_level": "info",
                        "framework": "actix"
                    }),
                    label: Some("main.rs".into()),
                    comment: Some("Actix Web HTTP Server Entry Point".into()),
                });

                // 2. MongoDB Node
                graph.nodes.push(Node {
                    id: NodeId("mongodb_client".into()),
                    template_id: TemplateId::new("marketplace.mongodb").unwrap(),
                    position: Position { x: -300.0, y: -200.0 },
                    config: serde_json::json!({
                        "uri": "mongodb://localhost:27017",
                        "database": "ecommerce_db",
                        "collection": "orders"
                    }),
                    label: Some("MongoDB Database".into()),
                    comment: Some("Visual MongoDB configuration".into()),
                });

                // 3. Redis Node
                graph.nodes.push(Node {
                    id: NodeId("redis_client".into()),
                    template_id: TemplateId::new("integration.redis").unwrap(),
                    position: Position { x: -300.0, y: -100.0 },
                    config: serde_json::json!({
                        "connection_string": "redis://127.0.0.1:6379",
                        "operation": "GET"
                    }),
                    label: Some("Redis Cache".into()),
                    comment: Some("Visual Redis configuration".into()),
                });

                // 4. DB Helper Handler (writes to src/handlers/db_helper.rs)
                let db_helper_code = r#"use once_cell::sync::Lazy;
use tokio::sync::OnceCell;
use mongodb::Client;

pub static MONGO: Lazy<OnceCell<Client>> = Lazy::new(OnceCell::new);
pub static REDIS: Lazy<OnceCell<redis::Client>> = Lazy::new(OnceCell::new);

pub async fn get_mongo() -> &'static Client {
    MONGO.get_or_init(|| async {
        match Client::with_uri_str("mongodb://localhost:27017").await {
            Ok(c) => c,
            Err(_) => std::process::exit(1),
        }
    }).await
}

pub async fn get_redis() -> &'static redis::Client {
    REDIS.get_or_init(|| async {
        match redis::Client::open("redis://127.0.0.1:6379") {
            Ok(c) => c,
            Err(_) => std::process::exit(1),
        }
    }).await
}

use actix_web::Responder;
use crate::errors::AppError;
pub async fn db_helper() -> Result<impl Responder, AppError> {
    Ok("DB Helper Initialized")
}
"#;

                graph.nodes.push(Node {
                    id: NodeId("db_helper".into()),
                    template_id: TemplateId::new("http.handler").unwrap(),
                    position: Position { x: -300.0, y: 0.0 },
                    config: serde_json::json!({
                        "name": "db_helper",
                        "code": db_helper_code
                    }),
                    label: Some("DB Connection Pool".into()),
                    comment: Some("Shared lazy static connections for Mongo & Redis".into()),
                });

                // Helper local structures for routes, handlers, and middlewares to wire
                let auth_mw_body = r#"let auth_header = req.headers().get("Authorization");
if let Some(auth_str) = auth_header.and_then(|h| h.to_str().ok()) {
    if auth_str.starts_with("Bearer ") {
        let token = &auth_str[7..];
        if token == "demo-token" || token.starts_with("eyJ") {
            return next.call(req).await;
        }
    }
}
Err(actix_web::error::ErrorUnauthorized("Invalid or missing Bearer token"))"#;

                struct RouteSpec {
                    id: &'static str,
                    path: &'static str,
                    method: &'static str,
                    handler_name: &'static str,
                    label: &'static str,
                    comment: &'static str,
                    code: &'static str,
                    y_offset: f64,
                    auth: bool,
                }

                let specs = vec![
                    RouteSpec {
                        id: "signup",
                        path: "/auth/signup",
                        method: "POST",
                        handler_name: "signup",
                        label: "User Signup",
                        comment: "Creates profile and seeds $10,000 USD wallet",
                        code: r#"use actix_web::{web, Responder, HttpResponse};
use crate::errors::AppError;
use crate::handlers::db_helper::get_mongo;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct SignupRequest {
    pub username: String,
    pub password: Option<String>,
}

#[derive(Serialize)]
pub struct SignupResponse {
    pub username: String,
    pub wallet: f64,
}

pub async fn signup(payload: web::Json<SignupRequest>) -> Result<impl Responder, AppError> {
    let client = get_mongo().await;
    let db = client.database("ecommerce_db");
    let coll = db.collection::<mongodb::bson::Document>("users");

    let filter = mongodb::bson::doc! { "username": &payload.username };
    if coll.find_one(filter, None).await.map_err(|e| AppError::Internal)?.is_some() {
        return Err(AppError::BadRequest("Username already exists".to_string()));
    }

    let password = payload.password.clone().unwrap_or_else(|| "password".to_string());
    let hashed = bcrypt::hash(password, bcrypt::DEFAULT_COST)
        .map_err(|e| AppError::Internal)?;

    let doc = mongodb::bson::doc! {
        "username": &payload.username,
        "password_hash": hashed,
        "wallet": 10000.0,
    };
    coll.insert_one(doc, None).await.map_err(|e| AppError::Internal)?;

    Ok(HttpResponse::Created().json(SignupResponse {
        username: payload.username.clone(),
        wallet: 10000.0,
    }))
}"#,
                        y_offset: 0.0,
                        auth: false,
                    },
                    RouteSpec {
                        id: "login",
                        path: "/auth/login",
                        method: "POST",
                        handler_name: "login",
                        label: "User Login",
                        comment: "Verifies profile and returns demo JWT Bearer token",
                        code: r#"use actix_web::{web, Responder, HttpResponse};
use crate::errors::AppError;
use crate::handlers::db_helper::get_mongo;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: Option<String>,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub token: String,
}

pub async fn login(payload: web::Json<LoginRequest>) -> Result<impl Responder, AppError> {
    let client = get_mongo().await;
    let db = client.database("ecommerce_db");
    let coll = db.collection::<mongodb::bson::Document>("users");

    let filter = mongodb::bson::doc! { "username": &payload.username };
    let user_doc = coll.find_one(filter, None).await
        .map_err(|e| AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest("User not found".to_string()))?;

    let hash = user_doc.get_str("password_hash")
        .map_err(|_| AppError::Internal)?;

    let password = payload.password.clone().unwrap_or_else(|| "password".to_string());
    if bcrypt::verify(password, hash).map_err(|e| AppError::Internal)? {
        Ok(HttpResponse::Ok().json(LoginResponse {
            token: "demo-token".to_string(),
        }))
    } else {
        Err(AppError::BadRequest("Invalid password".to_string()))
    }
}"#,
                        y_offset: 120.0,
                        auth: false,
                    },
                    RouteSpec {
                        id: "create_item",
                        path: "/items",
                        method: "POST",
                        handler_name: "create_item",
                        label: "Create SKU Item",
                        comment: "Multipart request uploading files onto local disk",
                        code: r#"use actix_web::{Responder, HttpResponse};
use actix_multipart::Multipart;
use futures::stream::TryStreamExt;
use crate::errors::AppError;
use crate::handlers::db_helper::{get_mongo, get_redis};
use serde::Serialize;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use redis::AsyncCommands;

#[derive(Serialize)]
pub struct ItemResponse {
    pub id: String,
    pub name: String,
    pub sku: String,
    pub category: String,
    pub price: f64,
    pub image_path: String,
}

pub async fn create_item(mut payload: Multipart) -> Result<impl Responder, AppError> {
    let mut name = String::new();
    let mut sku = String::new();
    let mut category = String::new();
    let mut price = 0.0;
    let mut file_path = String::new();

    tokio::fs::create_dir_all("uploads").await.map_err(|_| AppError::Internal)?;

    while let Some(mut field) = payload.try_next().await.map_err(|e| AppError::BadRequest(e.to_string()))? {
        let content_disposition = field.content_disposition();
        let name_field = content_disposition.get_name().unwrap_or("");

        if name_field == "image" {
            let filename = content_disposition.get_filename().unwrap_or("image.jpg").to_string();
            let dest_path = format!("uploads/{}", filename);
            file_path = dest_path.clone();

            let mut f = File::create(&dest_path).await.map_err(|_| AppError::Internal)?;
            while let Some(chunk) = field.try_next().await.map_err(|e| AppError::BadRequest(e.to_string()))? {
                f.write_all(&chunk).await.map_err(|_| AppError::Internal)?;
            }
        } else {
            let mut value_bytes = Vec::new();
            while let Some(chunk) = field.try_next().await.map_err(|e| AppError::BadRequest(e.to_string()))? {
                value_bytes.extend_from_slice(&chunk);
            }
            let value_str = String::from_utf8(value_bytes).map_err(|_| AppError::BadRequest("Invalid UTF8".to_string()))?;
            match name_field {
                "name" => name = value_str,
                "sku" => sku = value_str,
                "category" => category = value_str,
                "price" => price = value_str.parse::<f64>().unwrap_or(0.0),
                _ => {}
            }
        }
    }

    let client = get_mongo().await;
    let db = client.database("ecommerce_db");
    let coll = db.collection::<mongodb::bson::Document>("items");

    let doc = mongodb::bson::doc! {
        "name": &name,
        "sku": &sku,
        "category": &category,
        "price": price,
        "image_path": &file_path,
    };
    let insert_result = coll.insert_one(doc, None).await.map_err(|_| AppError::Internal)?;
    let id_str = insert_result.inserted_id.as_object_id()
        .map(|oid| oid.to_hex())
        .ok_or_else(|| AppError::Internal)?;

    if let Ok(mut conn) = get_redis().await.get_tokio_connection().await {
        let _: Result<(), redis::RedisError> = conn.del("items:all").await;
    }

    Ok(HttpResponse::Created().json(ItemResponse {
        id: id_str,
        name,
        sku,
        category,
        price,
        image_path: file_path,
    }))
}"#,
                        y_offset: 240.0,
                        auth: false,
                    },
                    RouteSpec {
                        id: "fetch_item",
                        path: "/items/:id",
                        method: "GET",
                        handler_name: "fetch_item",
                        label: "Fetch SKU Item",
                        comment: "Read-aside caching logic over Redis database fallback",
                        code: r#"use actix_web::{web, Responder, HttpResponse};
use crate::errors::AppError;
use crate::handlers::db_helper::{get_mongo, get_redis};
use serde::{Deserialize, Serialize};
use redis::AsyncCommands;

#[derive(Serialize, Deserialize, Clone)]
pub struct ItemResponse {
    pub id: String,
    pub name: String,
    pub sku: String,
    pub category: String,
    pub price: f64,
    pub image_path: String,
}

pub async fn fetch_item(path: web::Path<String>) -> Result<impl Responder, AppError> {
    let item_id = path.into_inner();
    let redis_key = format!("item:{}", item_id);

    if let Ok(mut conn) = get_redis().await.get_tokio_connection().await {
        if let Ok(cached_str) = conn.get::<_, String>(&redis_key).await {
            if let Ok(item) = serde_json::from_str::<ItemResponse>(&cached_str) {
                return Ok(HttpResponse::Ok().json(item));
            }
        }
    }

    let client = get_mongo().await;
    let db = client.database("ecommerce_db");
    let coll = db.collection::<mongodb::bson::Document>("items");

    let obj_id = mongodb::bson::oid::ObjectId::parse_str(&item_id)
        .map_err(|_| AppError::BadRequest("Invalid ID format".to_string()))?;
    let filter = mongodb::bson::doc! { "_id": obj_id };

    let doc = coll.find_one(filter, None).await.map_err(|_| AppError::Internal)?
        .ok_or(AppError::NotFound)?;

    let item = ItemResponse {
        id: item_id.clone(),
        name: doc.get_str("name").unwrap_or("").to_string(),
        sku: doc.get_str("sku").unwrap_or("").to_string(),
        category: doc.get_str("category").unwrap_or("").to_string(),
        price: doc.get_f64("price").unwrap_or(0.0),
        image_path: doc.get_str("image_path").unwrap_or("").to_string(),
    };

    if let Ok(mut conn) = get_redis().await.get_tokio_connection().await {
        if let Ok(serialized) = serde_json::to_string(&item) {
            let _: Result<(), redis::RedisError> = conn.set_ex(&redis_key, serialized, 60).await;
        }
    }

    Ok(HttpResponse::Ok().json(item))
}"#,
                        y_offset: 360.0,
                        auth: false,
                    },
                    RouteSpec {
                        id: "delete_item",
                        path: "/items/:id",
                        method: "DELETE",
                        handler_name: "delete_item",
                        label: "Delete SKU Item",
                        comment: "Deletes document and purges keys inside Redis cache",
                        code: r#"use actix_web::{web, Responder, HttpResponse};
use crate::errors::AppError;
use crate::handlers::db_helper::{get_mongo, get_redis};
use redis::AsyncCommands;

pub async fn delete_item(path: web::Path<String>) -> Result<impl Responder, AppError> {
    let item_id = path.into_inner();
    let redis_key = format!("item:{}", item_id);

    let client = get_mongo().await;
    let db = client.database("ecommerce_db");
    let coll = db.collection::<mongodb::bson::Document>("items");

    let obj_id = mongodb::bson::oid::ObjectId::parse_str(&item_id)
        .map_err(|_| AppError::BadRequest("Invalid ID format".to_string()))?;
    let filter = mongodb::bson::doc! { "_id": obj_id };

    coll.delete_one(filter, None).await.map_err(|_| AppError::Internal)?;

    if let Ok(mut conn) = get_redis().await.get_tokio_connection().await {
        let _: Result<(), redis::RedisError> = conn.del(&redis_key).await;
    }

    Ok(HttpResponse::NoContent().finish())
}"#,
                        y_offset: 480.0,
                        auth: false,
                    },
                    RouteSpec {
                        id: "create_order",
                        path: "/orders",
                        method: "POST",
                        handler_name: "create_order",
                        label: "Place Order",
                        comment: "Multi-document Mongo transaction with write conflict retries",
                        code: r#"use actix_web::{web, Responder, HttpResponse};
use crate::errors::AppError;
use crate::handlers::db_helper::get_mongo;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct OrderRequest {
    pub item_id: String,
    pub quantity: i32,
    pub username: String,
}

#[derive(Serialize)]
pub struct OrderResponse {
    pub order_id: String,
    pub username: String,
    pub item_id: String,
    pub total_price: f64,
    pub remaining_balance: f64,
}

pub async fn create_order(payload: web::Json<OrderRequest>) -> Result<impl Responder, AppError> {
    let client = get_mongo().await;
    let db = client.database("ecommerce_db");

    let mut retries = 5;
    while retries > 0 {
        let mut session = client.start_session(None).await.map_err(|_| AppError::Internal)?;
        session.start_transaction(None).await.map_err(|_| AppError::Internal)?;

        let result: Result<OrderResponse, AppError> = async {
            let items_coll = db.collection::<mongodb::bson::Document>("items");
            let users_coll = db.collection::<mongodb::bson::Document>("users");
            let orders_coll = db.collection::<mongodb::bson::Document>("orders");

            let item_obj_id = mongodb::bson::oid::ObjectId::parse_str(&payload.item_id)
                .map_err(|_| AppError::BadRequest("Invalid Item ID format".to_string()))?;
            let item_doc = items_coll.find_one_with_session(mongodb::bson::doc! { "_id": item_obj_id }, None, &mut session).await
                .map_err(|_| AppError::Internal)?
                .ok_or(AppError::NotFound)?;
            
            let price = item_doc.get_f64("price").unwrap_or(0.0);
            let total_price = price * (payload.quantity as f64);

            let user_doc = users_coll.find_one_with_session(mongodb::bson::doc! { "username": &payload.username }, None, &mut session).await
                .map_err(|_| AppError::Internal)?
                .ok_or_else(|| AppError::BadRequest("User not found".to_string()))?;

            let mut balance = user_doc.get_f64("wallet").unwrap_or(0.0);
            if balance < total_price {
                return Err(AppError::BadRequest("Insufficient wallet balance".to_string()));
            }

            balance -= total_price;
            users_coll.update_one_with_session(
                mongodb::bson::doc! { "username": &payload.username },
                mongodb::bson::doc! { "$set": { "wallet": balance } },
                None,
                &mut session
            ).await.map_err(|_| AppError::Internal)?;

            let order_doc = mongodb::bson::doc! {
                "username": &payload.username,
                "item_id": &payload.item_id,
                "quantity": payload.quantity,
                "total_price": total_price,
                "status": "Paid",
            };
            let insert_res = orders_coll.insert_one_with_session(order_doc, None, &mut session).await
                .map_err(|_| AppError::Internal)?;
            let order_id = insert_res.inserted_id.as_object_id()
                .map(|oid| oid.to_hex())
                .ok_or_else(|| AppError::Internal)?;

            Ok(OrderResponse {
                order_id,
                username: payload.username.clone(),
                item_id: payload.item_id.clone(),
                total_price,
                remaining_balance: balance,
            })
        }.await;

        match result {
            Ok(resp) => {
                session.commit_transaction().await.map_err(|_| AppError::Internal)?;
                return Ok(HttpResponse::Ok().json(resp));
            }
            Err(e) => {
                session.abort_transaction().await.map_err(|_| AppError::Internal)?;
                if e.to_string().contains("WriteConflict") {
                    retries -= 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                } else {
                    return Err(e);
                }
            }
        }
    }

    Err(AppError::Internal)
}"#,
                        y_offset: 600.0,
                        auth: true,
                    },
                    RouteSpec {
                        id: "fetch_order",
                        path: "/orders/:id",
                        method: "GET",
                        handler_name: "fetch_order",
                        label: "Fetch Single Order",
                        comment: "Retrieves order documents for user by order ID",
                        code: r#"use actix_web::{web, Responder, HttpResponse};
use crate::errors::AppError;
use crate::handlers::db_helper::get_mongo;
use serde::Serialize;

#[derive(Serialize)]
pub struct OrderView {
    pub id: String,
    pub username: String,
    pub item_id: String,
    pub quantity: i32,
    pub total_price: f64,
    pub status: String,
}

pub async fn fetch_order(path: web::Path<String>) -> Result<impl Responder, AppError> {
    let order_id = path.into_inner();
    let client = get_mongo().await;
    let db = client.database("ecommerce_db");
    let coll = db.collection::<mongodb::bson::Document>("orders");

    let obj_id = mongodb::bson::oid::ObjectId::parse_str(&order_id)
        .map_err(|_| AppError::BadRequest("Invalid ID format".to_string()))?;
    let filter = mongodb::bson::doc! { "_id": obj_id };

    let doc = coll.find_one(filter, None).await.map_err(|_| AppError::Internal)?
        .ok_or(AppError::NotFound)?;

    Ok(HttpResponse::Ok().json(OrderView {
        id: order_id,
        username: doc.get_str("username").unwrap_or("").to_string(),
        item_id: doc.get_str("item_id").unwrap_or("").to_string(),
        quantity: doc.get_i32("quantity").unwrap_or(0),
        total_price: doc.get_f64("total_price").unwrap_or(0.0),
        status: doc.get_str("status").unwrap_or("").to_string(),
    }))
}"#,
                        y_offset: 720.0,
                        auth: true,
                    },
                    RouteSpec {
                        id: "list_orders",
                        path: "/orders",
                        method: "GET",
                        handler_name: "list_orders",
                        label: "List Active Orders",
                        comment: "Lists transaction database histories matching user profile",
                        code: r#"use actix_web::{web, Responder, HttpResponse};
use crate::errors::AppError;
use crate::handlers::db_helper::get_mongo;
use serde::{Deserialize, Serialize};
use futures::stream::TryStreamExt;

#[derive(Deserialize)]
pub struct ListOrdersQuery {
    pub username: String,
}

#[derive(Serialize)]
pub struct OrderView {
    pub id: String,
    pub username: String,
    pub item_id: String,
    pub quantity: i32,
    pub total_price: f64,
    pub status: String,
}

pub async fn list_orders(query: web::Query<ListOrdersQuery>) -> Result<impl Responder, AppError> {
    let client = get_mongo().await;
    let db = client.database("ecommerce_db");
    let coll = db.collection::<mongodb::bson::Document>("orders");

    let filter = mongodb::bson::doc! { "username": &query.username };
    let mut cursor = coll.find(filter, None).await.map_err(|_| AppError::Internal)?;
    let mut orders = Vec::new();

    while let Some(doc) = cursor.try_next().await.map_err(|_| AppError::Internal)? {
        orders.push(OrderView {
            id: doc.get_object_id("_id").map(|oid| oid.to_hex()).unwrap_or_default(),
            username: doc.get_str("username").unwrap_or("").to_string(),
            item_id: doc.get_str("item_id").unwrap_or("").to_string(),
            quantity: doc.get_i32("quantity").unwrap_or(0),
            total_price: doc.get_f64("total_price").unwrap_or(0.0),
            status: doc.get_str("status").unwrap_or("").to_string(),
        });
    }

    Ok(HttpResponse::Ok().json(orders))
}"#,
                        y_offset: 840.0,
                        auth: true,
                    },
                    RouteSpec {
                        id: "deposit",
                        path: "/wallet/deposit",
                        method: "POST",
                        handler_name: "deposit",
                        label: "Deposit Wallet",
                        comment: "Credits user wallet balance inside the MongoDB store",
                        code: r#"use actix_web::{web, Responder, HttpResponse};
use crate::errors::AppError;
use crate::handlers::db_helper::get_mongo;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct DepositRequest {
    pub username: String,
    pub amount: f64,
}

#[derive(Serialize)]
pub struct DepositResponse {
    pub username: String,
    pub updated_balance: f64,
}

pub async fn deposit(payload: web::Json<DepositRequest>) -> Result<impl Responder, AppError> {
    let client = get_mongo().await;
    let db = client.database("ecommerce_db");
    let coll = db.collection::<mongodb::bson::Document>("users");

    let filter = mongodb::bson::doc! { "username": &payload.username };
    let user_doc = coll.find_one(filter.clone(), None).await.map_err(|_| AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest("User not found".to_string()))?;

    let mut balance = user_doc.get_f64("wallet").unwrap_or(0.0);
    balance += payload.amount;

    coll.update_one(
        filter,
        mongodb::bson::doc! { "$set": { "wallet": balance } },
        None
    ).await.map_err(|_| AppError::Internal)?;

    Ok(HttpResponse::Ok().json(DepositResponse {
        username: payload.username.clone(),
        updated_balance: balance,
    }))
}"#,
                        y_offset: 960.0,
                        auth: true,
                    },
                    RouteSpec {
                        id: "get_wallet",
                        path: "/wallet",
                        method: "GET",
                        handler_name: "get_wallet",
                        label: "Get Wallet Balance",
                        comment: "Fetches user balance directly from user document",
                        code: r#"use actix_web::{web, Responder, HttpResponse};
use crate::errors::AppError;
use crate::handlers::db_helper::get_mongo;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct WalletQuery {
    pub username: String,
}

#[derive(Serialize)]
pub struct WalletResponse {
    pub username: String,
    pub wallet: f64,
}

pub async fn get_wallet(query: web::Query<WalletQuery>) -> Result<impl Responder, AppError> {
    let client = get_mongo().await;
    let db = client.database("ecommerce_db");
    let coll = db.collection::<mongodb::bson::Document>("users");

    let filter = mongodb::bson::doc! { "username": &query.username };
    let user_doc = coll.find_one(filter, None).await.map_err(|_| AppError::Internal)?
        .ok_or_else(|| AppError::BadRequest("User not found".to_string()))?;

    let balance = user_doc.get_f64("wallet").unwrap_or(0.0);

    Ok(HttpResponse::Ok().json(WalletResponse {
        username: query.username.clone(),
        wallet: balance,
    }))
}"#,
                        y_offset: 1080.0,
                        auth: true,
                    },
                ];

                for spec in specs {
                    // Create Route Node
                    graph.nodes.push(Node {
                        id: NodeId(format!("route_{}", spec.id)),
                        template_id: TemplateId::new("http.route").unwrap(),
                        position: Position { x: 250.0, y: spec.y_offset },
                        config: serde_json::json!({
                            "path": spec.path,
                            "method": spec.method
                        }),
                        label: Some(spec.label.to_string()),
                        comment: Some(spec.comment.to_string()),
                    });

                    // Wire Entry Point to Route Node
                    graph.edges.push(Edge {
                        id: EdgeId(format!("edge_entry_to_{}", spec.id)),
                        source: NodeId("entry".into()),
                        target: NodeId(format!("route_{}", spec.id)),
                        source_port: "http".to_string(),
                        target_port: "entry".to_string(),
                    });

                    // Create Handler Node
                    graph.nodes.push(Node {
                        id: NodeId(format!("handler_{}", spec.id)),
                        template_id: TemplateId::new("http.handler").unwrap(),
                        position: Position { x: if spec.auth { 750.0 } else { 550.0 }, y: spec.y_offset },
                        config: serde_json::json!({
                            "name": spec.handler_name,
                            "code": spec.code
                        }),
                        label: Some(format!("{} Handler", spec.label)),
                        comment: None,
                    });

                    if spec.auth {
                        // Create Middleware Node for authentication
                        let mw_name = format!("auth_mw_{}", spec.id);
                        graph.nodes.push(Node {
                            id: NodeId(format!("mw_{}", spec.id)),
                            template_id: TemplateId::new("http.middleware").unwrap(),
                            position: Position { x: 500.0, y: spec.y_offset },
                            config: serde_json::json!({
                                "name": mw_name,
                                "body": auth_mw_body
                            }),
                            label: Some(format!("Auth Middleware ({})", spec.id)),
                            comment: Some("Bearer authorization verification".into()),
                        });

                        // Wire Route -> Middleware
                        graph.edges.push(Edge {
                            id: EdgeId(format!("edge_route_to_mw_{}", spec.id)),
                            source: NodeId(format!("route_{}", spec.id)),
                            target: NodeId(format!("mw_{}", spec.id)),
                            source_port: "request".to_string(),
                            target_port: "request".to_string(),
                        });

                        // Wire Middleware -> Handler
                        graph.edges.push(Edge {
                            id: EdgeId(format!("edge_mw_to_handler_{}", spec.id)),
                            source: NodeId(format!("mw_{}", spec.id)),
                            target: NodeId(format!("handler_{}", spec.id)),
                            source_port: "handler".to_string(),
                            target_port: "request".to_string(),
                        });
                    } else {
                        // Wire Route -> Handler Directly
                        graph.edges.push(Edge {
                            id: EdgeId(format!("edge_route_to_handler_{}", spec.id)),
                            source: NodeId(format!("route_{}", spec.id)),
                            target: NodeId(format!("handler_{}", spec.id)),
                            source_port: "request".to_string(),
                            target_port: "request".to_string(),
                        });
                    }
                }
            }
            "order_processor" => {
                packages.push("mongodb".to_string());
                packages.push("redis".to_string());

                // 1. Entry Point
                graph.nodes.push(Node {
                    id: NodeId("entry".into()),
                    template_id: TemplateId::new("core.entry_point").unwrap(),
                    position: Position { x: 0.0, y: 150.0 },
                    config: serde_json::json!({
                        "bind_address": "127.0.0.1:8080",
                        "log_level": "info",
                        "framework": "axum"
                    }),
                    label: Some("main.rs".into()),
                    comment: Some("Axum Web HTTP Server Entry Point".into()),
                });

                // 2. DTO: OrderDto
                graph.nodes.push(Node {
                    id: NodeId("order_dto".into()),
                    template_id: TemplateId::new("core.dto").unwrap(),
                    position: Position { x: -350.0, y: -200.0 },
                    config: serde_json::json!({
                        "name": "OrderDto",
                        "fields": [
                            { "name": "id", "ty": "u64" },
                            { "name": "item", "ty": "String" },
                            { "name": "amount", "ty": "f64" },
                            { "name": "status", "ty": "String" }
                        ]
                    }),
                    label: Some("OrderDto".into()),
                    comment: Some("Defines structured Order format".into()),
                });

                // 3. HTTP Route: POST /orders
                graph.nodes.push(Node {
                    id: NodeId("route_create".into()),
                    template_id: TemplateId::new("http.route").unwrap(),
                    position: Position { x: 250.0, y: 0.0 },
                    config: serde_json::json!({
                        "path": "/orders",
                        "method": "POST"
                    }),
                    label: Some("POST /orders".into()),
                    comment: Some("Create a new Order".into()),
                });

                // 4. HTTP Handler: create_order
                let create_order_code = r#"use axum::{Json, response::IntoResponse};
use crate::errors::AppError;
use crate::functions::check_amount;
use crate::integrations::db_writer;
use crate::integrations::redis_client;
use crate::integrations::kafka_producer;

#[derive(serde::Deserialize)]
pub struct OrderRequest {
    pub item: String,
    pub amount: f64,
}

pub async fn create_order(Json(payload): Json<OrderRequest>) -> Result<impl IntoResponse, AppError> {
    // 1. Visually check if amount is premium using the If/Else node
    let level = check_amount::check_amount(payload.amount);
    
    // 2. Write order to SQLite database
    let order_id = 12345u64;
    let params = vec![
        order_id.to_string(),
        payload.item.clone(),
        payload.amount.to_string(),
        level.clone(),
    ];
    let _ = db_writer::execute(params).await?;

    // 3. Cache the order status in Redis
    let cache_status = format!("ID: {}, Level: {}", order_id, level);
    let _ = redis_client::execute(order_id.to_string(), cache_status).await?;

    // 4. Publish order created event to Kafka
    let event = format!("ORDER_CREATED: {}, amount: {}", order_id, payload.amount);
    let _ = kafka_producer::send_message(event).await?;

    Ok(Json(serde_json::json!({
        "status": "success",
        "order_id": order_id,
        "item": payload.item,
        "amount": payload.amount,
        "level": level
    })))
}
"#;
                graph.nodes.push(Node {
                    id: NodeId("handler_create".into()),
                    template_id: TemplateId::new("http.handler").unwrap(),
                    position: Position { x: 500.0, y: 0.0 },
                    config: serde_json::json!({
                        "name": "create_order",
                        "code": create_order_code
                    }),
                    label: Some("Create Order Handler".into()),
                    comment: Some("Coordinates order pipeline visually".into()),
                });

                // 5. Visual If/Else: check_amount (checks amount > 100.0)
                graph.nodes.push(Node {
                    id: NodeId("check_amount".into()),
                    template_id: TemplateId::new("language.if_else").unwrap(),
                    position: Position { x: 750.0, y: -100.0 },
                    config: serde_json::json!({
                        "name": "check_amount",
                        "condition": "create_order_request > 100.0",
                        "true_expr": "\"premium\".to_string()",
                        "false_expr": "\"standard\".to_string()",
                        "input_type": "f64",
                        "return_type": "String"
                    }),
                    label: Some("Check Premium Amount".into()),
                    comment: Some("Visual conditional check (amount > 100.0)".into()),
                });

                // 6. SQL Database Writer: db_writer (SQLite database)
                graph.nodes.push(Node {
                    id: NodeId("db_writer".into()),
                    template_id: TemplateId::new("integration.db_writer").unwrap(),
                    position: Position { x: 750.0, y: 100.0 },
                    config: serde_json::json!({
                        "db_path": "orders.db",
                        "query": "INSERT INTO orders (id, item, amount, level) VALUES (?, ?, ?, ?)",
                        "name": "db_writer"
                    }),
                    label: Some("SQLite Database".into()),
                    comment: Some("SQLite local persistence".into()),
                });

                // 7. Redis client: redis_client
                graph.nodes.push(Node {
                    id: NodeId("redis_client".into()),
                    template_id: TemplateId::new("integration.redis").unwrap(),
                    position: Position { x: 1000.0, y: -100.0 },
                    config: serde_json::json!({
                        "connection_string": "redis://127.0.0.1:6379",
                        "operation": "SET",
                        "name": "redis_client"
                    }),
                    label: Some("Redis Cache".into()),
                    comment: Some("Redis key/value storage".into()),
                });

                // 8. Kafka Publisher: kafka_producer
                graph.nodes.push(Node {
                    id: NodeId("kafka_producer".into()),
                    template_id: TemplateId::new("integration.kafka_producer").unwrap(),
                    position: Position { x: 1000.0, y: 100.0 },
                    config: serde_json::json!({
                        "brokers": "localhost:9092",
                        "topic": "order_events",
                        "name": "kafka_producer"
                    }),
                    label: Some("Kafka Publisher".into()),
                    comment: Some("Emits order events to Kafka".into()),
                });

                // 9. Kafka Consumer background task: kafka_consumer
                graph.nodes.push(Node {
                    id: NodeId("kafka_consumer".into()),
                    template_id: TemplateId::new("integration.kafka_consumer").unwrap(),
                    position: Position { x: -350.0, y: 350.0 },
                    config: serde_json::json!({
                        "brokers": "localhost:9092",
                        "topic": "order_events",
                        "group": "processor_group",
                        "name": "kafka_consumer"
                    }),
                    label: Some("Kafka Consumer Worker".into()),
                    comment: Some("Background worker polling order confirmation events".into()),
                });

                // 10. Scheduler: cleanup_cron (runs midnight cleanup)
                graph.nodes.push(Node {
                    id: NodeId("cleanup_cron".into()),
                    template_id: TemplateId::new("integration.scheduler").unwrap(),
                    position: Position { x: -350.0, y: 550.0 },
                    config: serde_json::json!({
                        "cron": "0 0 0 * * *",
                        "name": "cleanup_scheduler"
                    }),
                    label: Some("Database Cleanup Cron".into()),
                    comment: Some("Triggers every midnight for data maintenance".into()),
                });

                // --- EDGES & CONNECTIONS ---

                // Connect Entry to Route
                graph.edges.push(Edge {
                    id: EdgeId("entry_to_route_create".into()),
                    source: NodeId("entry".into()),
                    target: NodeId("route_create".into()),
                    source_port: "http".to_string(),
                    target_port: "entry".to_string(),
                });

                // Connect Route to Handler
                graph.edges.push(Edge {
                    id: EdgeId("route_to_handler_create".into()),
                    source: NodeId("route_create".into()),
                    target: NodeId("handler_create".into()),
                    source_port: "request".to_string(),
                    target_port: "request".to_string(),
                });

                // Connect Handler to Visual If/Else
                graph.edges.push(Edge {
                    id: EdgeId("handler_to_ifelse".into()),
                    source: NodeId("handler_create".into()),
                    target: NodeId("check_amount".into()),
                    source_port: "response".to_string(),
                    target_port: "input".to_string(),
                });

                // Connect Handler to SQLite Writer
                graph.edges.push(Edge {
                    id: EdgeId("handler_to_sqlite".into()),
                    source: NodeId("handler_create".into()),
                    target: NodeId("db_writer".into()),
                    source_port: "response".to_string(),
                    target_port: "params".to_string(),
                });

                // Connect Handler to Redis Cache
                graph.edges.push(Edge {
                    id: EdgeId("handler_to_redis".into()),
                    source: NodeId("handler_create".into()),
                    target: NodeId("redis_client".into()),
                    source_port: "response".to_string(),
                    target_port: "key".to_string(),
                });

                // Connect Handler to Kafka Producer
                graph.edges.push(Edge {
                    id: EdgeId("handler_to_kafka_producer".into()),
                    source: NodeId("handler_create".into()),
                    target: NodeId("kafka_producer".into()),
                    source_port: "response".to_string(),
                    target_port: "payload".to_string(),
                });

                // Connect Entry point to Consumer Worker to spawn it
                graph.edges.push(Edge {
                    id: EdgeId("entry_to_kafka_consumer".into()),
                    source: NodeId("entry".into()),
                    target: NodeId("kafka_consumer".into()),
                    source_port: "consumer".to_string(),
                    target_port: "entry".to_string(),
                });

                // Connect Entry point to Scheduler to spawn it
                graph.edges.push(Edge {
                    id: EdgeId("entry_to_scheduler".into()),
                    source: NodeId("entry".into()),
                    target: NodeId("cleanup_cron".into()),
                    source_port: "scheduler".to_string(),
                    target_port: "entry".to_string(),
                });
            }
            "task_manager" => {
                // 1. Entry Point
                graph.nodes.push(Node {
                    id: NodeId("entry".into()),
                    template_id: TemplateId::new("core.entry_point").unwrap(),
                    position: Position { x: 0.0, y: 150.0 },
                    config: serde_json::json!({
                        "bind_address": "127.0.0.1:8080",
                        "log_level": "info",
                        "framework": "axum"
                    }),
                    label: Some("main.rs".into()),
                    comment: Some("Axum Web HTTP Server Entry Point".into()),
                });

                // 2. DTO: TaskDto
                graph.nodes.push(Node {
                    id: NodeId("task_dto".into()),
                    template_id: TemplateId::new("core.dto").unwrap(),
                    position: Position { x: -350.0, y: -200.0 },
                    config: serde_json::json!({
                        "name": "TaskDto",
                        "fields": [
                            { "name": "id", "ty": "u64" },
                            { "name": "title", "ty": "String" },
                            { "name": "completed", "ty": "bool" }
                        ]
                    }),
                    label: Some("TaskDto".into()),
                    comment: Some("Defines structured Task return format".into()),
                });

                // 3. DTO: TaskCreateDto
                graph.nodes.push(Node {
                    id: NodeId("task_create_dto".into()),
                    template_id: TemplateId::new("core.dto").unwrap(),
                    position: Position { x: -350.0, y: -50.0 },
                    config: serde_json::json!({
                        "name": "TaskCreateDto",
                        "fields": [
                            { "name": "title", "ty": "String" }
                        ]
                    }),
                    label: Some("TaskCreateDto".into()),
                    comment: Some("Defines input format to create a task".into()),
                });

                // 4. Service: task_store (In-Memory Database & thread-safe tasks pool)
                let store_code = r#"use crate::errors::AppError;
use std::sync::Mutex;
use once_cell::sync::Lazy;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Task {
    pub id: u64,
    pub title: String,
    pub completed: bool,
}

static TASKS: Lazy<Mutex<Vec<Task>>> = Lazy::new(|| Mutex::new(vec![
    Task { id: 1, title: "Learn async Rust".to_string(), completed: false },
    Task { id: 2, title: "Create visuals in UI canvas".to_string(), completed: true }
]));

pub async fn get_tasks() -> Result<Vec<Task>, AppError> {
    let list = TASKS.lock().map_err(|_| AppError::Internal)?.clone();
    Ok(list)
}

pub async fn add_task(title: String) -> Result<Task, AppError> {
    let mut list = TASKS.lock().map_err(|_| AppError::Internal)?;
    let next_id = list.iter().map(|t| t.id).max().unwrap_or(0) + 1;
    let task = Task { id: next_id, title, completed: false };
    list.push(task.clone());
    Ok(task)
}
"#;
                graph.nodes.push(Node {
                    id: NodeId("task_store".into()),
                    template_id: TemplateId::new("core.service").unwrap(),
                    position: Position { x: -350.0, y: 150.0 },
                    config: serde_json::json!({
                        "name": "task_store",
                        "description": "In-Memory thread-safe task storage service",
                        "code": store_code
                    }),
                    label: Some("In-Memory Store".into()),
                    comment: Some("Global Mutex task manager database".into()),
                });

                // 5. Auth Middleware
                let auth_mw_body = r#"let auth_header = req.headers().get("Authorization");
if let Some(auth_str) = auth_header.and_then(|h| h.to_str().ok()) {
    if auth_str == "Bearer secret-token" {
        return Ok(next.run(req).await);
    }
}
let mut res = axum::response::Response::new(axum::body::Body::from("Unauthorized Bearer Token"));
*res.status_mut() = axum::http::StatusCode::UNAUTHORIZED;
Err(res)"#;

                graph.nodes.push(Node {
                    id: NodeId("auth_mw".into()),
                    template_id: TemplateId::new("http.middleware").unwrap(),
                    position: Position { x: 500.0, y: 0.0 },
                    config: serde_json::json!({
                        "name": "auth_middleware",
                        "body": auth_mw_body
                    }),
                    label: Some("Bearer Token Security".into()),
                    comment: Some("Validates Authorization header".into()),
                });

                // 6. Router: POST /tasks
                graph.nodes.push(Node {
                    id: NodeId("route_create".into()),
                    template_id: TemplateId::new("http.route").unwrap(),
                    position: Position { x: 250.0, y: 0.0 },
                    config: serde_json::json!({
                        "path": "/tasks",
                        "method": "POST"
                    }),
                    label: Some("POST /tasks".into()),
                    comment: Some("Create a new task (Authenticated)".into()),
                });

                // 7. Handler: create_task
                let create_task_code = r#"use axum::{Json, response::IntoResponse};
use crate::errors::AppError;
use crate::services::task_store;

#[derive(serde::Deserialize)]
pub struct CreateRequest {
    pub title: String,
}

pub async fn create_task(Json(payload): Json<CreateRequest>) -> Result<impl IntoResponse, AppError> {
    let task = task_store::add_task(payload.title).await?;
    Ok(Json(task))
}
"#;
                graph.nodes.push(Node {
                    id: NodeId("handler_create".into()),
                    template_id: TemplateId::new("http.handler").unwrap(),
                    position: Position { x: 750.0, y: 0.0 },
                    config: serde_json::json!({
                        "name": "create_task",
                        "code": create_task_code
                    }),
                    label: Some("Create Task Handler".into()),
                    comment: None,
                });

                // 8. Router: GET /tasks
                graph.nodes.push(Node {
                    id: NodeId("route_list".into()),
                    template_id: TemplateId::new("http.route").unwrap(),
                    position: Position { x: 250.0, y: 250.0 },
                    config: serde_json::json!({
                        "path": "/tasks",
                        "method": "GET"
                    }),
                    label: Some("GET /tasks".into()),
                    comment: Some("List all tasks (Public)".into()),
                });

                // 9. Handler: list_tasks
                let list_tasks_code = r#"use axum::{Json, response::IntoResponse};
use crate::errors::AppError;
use crate::services::task_store;

pub async fn list_tasks() -> Result<impl IntoResponse, AppError> {
    let tasks = task_store::get_tasks().await?;
    Ok(Json(tasks))
}
"#;
                graph.nodes.push(Node {
                    id: NodeId("handler_list".into()),
                    template_id: TemplateId::new("http.handler").unwrap(),
                    position: Position { x: 550.0, y: 250.0 },
                    config: serde_json::json!({
                        "name": "list_tasks",
                        "code": list_tasks_code
                    }),
                    label: Some("List Tasks Handler".into()),
                    comment: None,
                });

                // --- EDGES & CONNECTIONS ---
                
                // Route entries to Entry point
                graph.edges.push(Edge {
                    id: EdgeId("entry_to_create".into()),
                    source: NodeId("entry".into()),
                    target: NodeId("route_create".into()),
                    source_port: "http".to_string(),
                    target_port: "entry".to_string(),
                });

                graph.edges.push(Edge {
                    id: EdgeId("entry_to_list".into()),
                    source: NodeId("entry".into()),
                    target: NodeId("route_list".into()),
                    source_port: "http".to_string(),
                    target_port: "entry".to_string(),
                });

                // POST Route -> Middleware -> Handler (Authenticated)
                graph.edges.push(Edge {
                    id: EdgeId("route_to_auth".into()),
                    source: NodeId("route_create".into()),
                    target: NodeId("auth_mw".into()),
                    source_port: "request".to_string(),
                    target_port: "request".to_string(),
                });

                graph.edges.push(Edge {
                    id: EdgeId("auth_to_handler".into()),
                    source: NodeId("auth_mw".into()),
                    target: NodeId("handler_create".into()),
                    source_port: "handler".to_string(),
                    target_port: "request".to_string(),
                });

                // GET Route -> Handler (Direct)
                graph.edges.push(Edge {
                    id: EdgeId("route_to_list_handler".into()),
                    source: NodeId("route_list".into()),
                    target: NodeId("handler_list".into()),
                    source_port: "request".to_string(),
                    target_port: "request".to_string(),
                });

                // DTO to Service mappings (purely visual mapping to DTO schemas)
                graph.edges.push(Edge {
                    id: EdgeId("dto_create_to_service".into()),
                    source: NodeId("task_create_dto".into()),
                    target: NodeId("task_store".into()),
                    source_port: "output".to_string(),
                    target_port: "input".to_string(),
                });

                graph.edges.push(Edge {
                    id: EdgeId("dto_response_to_service".into()),
                    source: NodeId("task_dto".into()),
                    target: NodeId("task_store".into()),
                    source_port: "output".to_string(),
                    target_port: "input".to_string(),
                });

                // Service to Handlers wiring (data connections)
                graph.edges.push(Edge {
                    id: EdgeId("service_to_create_handler".into()),
                    source: NodeId("task_store".into()),
                    target: NodeId("handler_create".into()),
                    source_port: "output".to_string(),
                    target_port: "request".to_string(),
                });

                graph.edges.push(Edge {
                    id: EdgeId("service_to_list_handler".into()),
                    source: NodeId("task_store".into()),
                    target: NodeId("handler_list".into()),
                    source_port: "output".to_string(),
                    target_port: "request".to_string(),
                });
            }
            "webrtc" => {
                packages.push("webrtc".to_string());
                packages.push("nats".to_string());

                // 1. Entry Point
                graph.nodes.push(Node {
                    id: NodeId("entry".into()),
                    template_id: TemplateId::new("core.entry_point").unwrap(),
                    position: Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({
                        "bind_address": "127.0.0.1:8080",
                        "log_level": "info",
                        "framework": "actix"
                    }),
                    label: Some("main.rs".into()),
                    comment: Some("Actix Web HTTP Server Entry Point".into()),
                });

                // 2. WebRTC Peer Connection
                graph.nodes.push(Node {
                    id: NodeId("webrtc_client".into()),
                    template_id: TemplateId::new("marketplace.webrtc").unwrap(),
                    position: Position { x: -300.0, y: 0.0 },
                    config: serde_json::json!({
                        "stun_server": "stun.l.google.com:19302"
                    }),
                    label: Some("WebRTC signaling server".into()),
                    comment: None,
                });

                // 3. NATS Client
                graph.nodes.push(Node {
                    id: NodeId("nats_client".into()),
                    template_id: TemplateId::new("marketplace.nats").unwrap(),
                    position: Position { x: -300.0, y: 150.0 },
                    config: serde_json::json!({
                        "url": "nats://127.0.0.1:4222",
                        "subject": "active_chat"
                    }),
                    label: Some("NATS PubSub Hub".into()),
                    comment: None,
                });
            }
            "analytics" => {
                packages.push("scylla".to_string());
                packages.push("clickhouse".to_string());

                // 1. Entry Point
                graph.nodes.push(Node {
                    id: NodeId("entry".into()),
                    template_id: TemplateId::new("core.entry_point").unwrap(),
                    position: Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({
                        "bind_address": "127.0.0.1:8080",
                        "log_level": "info",
                        "framework": "actix"
                    }),
                    label: Some("main.rs".into()),
                    comment: Some("Actix Web HTTP Server Entry Point".into()),
                });

                // 2. ScyllaDB Client
                graph.nodes.push(Node {
                    id: NodeId("scylla_client".into()),
                    template_id: TemplateId::new("marketplace.scylladb").unwrap(),
                    position: Position { x: -300.0, y: 0.0 },
                    config: serde_json::json!({
                        "uri": "127.0.0.1:9042",
                        "keyspace": "analytics",
                        "table": "pageviews"
                    }),
                    label: Some("ScyllaDB Columnar".into()),
                    comment: None,
                });

                // 3. ClickHouse Analytics Client
                graph.nodes.push(Node {
                    id: NodeId("clickhouse_client".into()),
                    template_id: TemplateId::new("marketplace.clickhouse").unwrap(),
                    position: Position { x: -300.0, y: 150.0 },
                    config: serde_json::json!({
                        "url": "http://localhost:8123",
                        "database": "reports"
                    }),
                    label: Some("ClickHouse Analytics".into()),
                    comment: None,
                });
            }
            "s3" => {
                packages.push("s3".to_string());
                packages.push("surrealdb".to_string());

                // 1. Entry Point
                graph.nodes.push(Node {
                    id: NodeId("entry".into()),
                    template_id: TemplateId::new("core.entry_point").unwrap(),
                    position: Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({
                        "bind_address": "127.0.0.1:8080",
                        "log_level": "info",
                        "framework": "actix"
                    }),
                    label: Some("main.rs".into()),
                    comment: Some("Actix Web HTTP Server Entry Point".into()),
                });

                // 2. AWS S3 Client
                graph.nodes.push(Node {
                    id: NodeId("s3_client".into()),
                    template_id: TemplateId::new("marketplace.s3").unwrap(),
                    position: Position { x: -300.0, y: 0.0 },
                    config: serde_json::json!({
                        "bucket": "cloud_assets",
                        "region": "us-east-1"
                    }),
                    label: Some("AWS S3 Cloud Storage".into()),
                    comment: None,
                });

                // 3. SurrealDB Client
                graph.nodes.push(Node {
                    id: NodeId("surreal_client".into()),
                    template_id: TemplateId::new("marketplace.surrealdb").unwrap(),
                    position: Position { x: -300.0, y: 150.0 },
                    config: serde_json::json!({
                        "endpoint": "mem://",
                        "namespace": "assets",
                        "database": "assets_db"
                    }),
                    label: Some("SurrealDB Graph DB".into()),
                    comment: None,
                });
            }
            "rabbitmq" => {
                packages.push("rabbitmq".to_string());

                // 1. Entry Point
                graph.nodes.push(Node {
                    id: NodeId("entry".into()),
                    template_id: TemplateId::new("core.entry_point").unwrap(),
                    position: Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({
                        "bind_address": "127.0.0.1:8080",
                        "log_level": "info",
                        "framework": "actix"
                    }),
                    label: Some("main.rs".into()),
                    comment: Some("Actix Web HTTP Server Entry Point".into()),
                });

                // 2. RabbitMQ Client
                graph.nodes.push(Node {
                    id: NodeId("rabbitmq_client".into()),
                    template_id: TemplateId::new("marketplace.rabbitmq").unwrap(),
                    position: Position { x: -300.0, y: 0.0 },
                    config: serde_json::json!({
                        "uri": "amqp://127.0.0.1:5672/%2f",
                        "queue": "background_tasks"
                    }),
                    label: Some("RabbitMQ Queue Broker".into()),
                    comment: None,
                });
            }
            _ => {
                // Default fallback: Single Entry Point
                graph.nodes.push(Node {
                    id: NodeId("entry".into()),
                    template_id: TemplateId::new("core.entry_point").unwrap(),
                    position: Position { x: 0.0, y: 0.0 },
                    config: serde_json::json!({"bind_address": "127.0.0.1:8080", "log_level": "info"}),
                    label: Some("main.rs".into()),
                    comment: None,
                });
            }
        }

        (graph, packages)
    }

    /// Create a new project with the given slug and display name.
    ///
    /// Atomic on the directory boundary: `tokio::fs::create_dir` fails with
    /// `AlreadyExists` if another caller (or a previous run) already
    /// created the folder. This is the *only* AlreadyExists barrier — we
    /// never rely on a check-then-act sequence.
    pub async fn create(&self, slug: Slug, name: String, template: Option<String>) -> Result<Project, ApiError> {
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
        // New projects start with a single root package (`main`). Future
        // user-driven CRUD (T3) attaches children under it; codegen (T4)
        // maps the root to `src/lib.rs` and children to nested modules.
        // The root id matches `LEGACY_ROOT_PACKAGE_ID` so a brand-new
        // project and a migrated legacy project share the same root id
        // shape — simplifies the eventual disk-migration assertions.
        let project = Project {
            meta: meta.clone(),
            packages: vec![Package {
                id: PackageId(crate::projects::types::LEGACY_ROOT_PACKAGE_ID.to_string()),
                slug: PackageSlug::root(),
                parent_id: None,
                label: None,
            }],
        };

        // Use the same atomic write helper that save_graph uses, so a crash
        // between create_dir and the project.json write leaves the dir but
        // no project.json — recoverable on retry (create() will see the
        // dir, hit AlreadyExists, and the operator can delete it manually).
        // Acceptable for v1.
        write_json_atomic(&dir.join(PROJECT_FILE), &project).await?;

        // Seed the visual graph and installed marketplace packages
        let (initial_graph, packages) = if let Some(ref t) = template {
            self.seed_template_graph(t)
        } else {
            let mut g = Graph::default();
            g.nodes.push(crate::projects::types::Node {
                id: crate::projects::types::NodeId("entry".into()),
                template_id: crate::templates::TemplateId::new("core.entry_point").unwrap(),
                position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({"bind_address": "127.0.0.1:8080", "log_level": "info"}),
                label: Some("main.rs".into()),
                comment: None,
            });
            (g, Vec::new())
        };

        // T2: graphs now live under `packages/<pkg-slug>/graph.json`. New
        // projects ship with the root package only; create its folder and
        // seed the initial graph there. T3 will add CRUD for child
        // packages.
        let root_pkg_dir = self.package_dir(&slug, &PackageSlug::root());
        fs::create_dir_all(&root_pkg_dir).await?;
        write_json_atomic(&root_pkg_dir.join(GRAPH_FILE), &initial_graph).await?;

        if !packages.is_empty() {
            write_json_atomic(&dir.join("marketplace.json"), &packages).await?;
        }

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

    /// Load the full project document (metadata + package tree; graph
    /// fragments are loaded separately via [`load_graph`] or
    /// [`load_graph_for_package`]).
    ///
    /// Side effects:
    /// 1. Validates the package tree before returning. A malformed tree
    ///    on disk surfaces as [`ApiError::InvalidBody`] so handlers see a
    ///    clean 422 rather than tripping later in codegen.
    /// 2. Runs the one-shot legacy-layout migration if needed (lifts a
    ///    pre-Section-1 root `graph.json` into `packages/main/`). The
    ///    migration is idempotent and adds two `fs::metadata` calls to
    ///    the steady-state load path.
    pub async fn load(&self, slug: &Slug) -> Result<Project, ApiError> {
        let path = self.project_dir(slug).join(PROJECT_FILE);
        let project: Project = read_json(&path).await?;
        project
            .validate_package_tree()
            .map_err(|e| ApiError::InvalidBody(format!("project.json: {e}")))?;
        self.migrate_legacy_graph_if_needed(slug).await?;
        Ok(project)
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

    /// Atomically mutate a project's metadata + package tree.
    ///
    /// The `mutator` closure receives an exclusive `&mut Project` taken
    /// from the on-disk file. The store calls
    /// [`Project::validate_package_tree`] on the result; if validation
    /// fails the closure's changes are discarded and the original
    /// `project.json` is left untouched.
    ///
    /// The per-slug write lock is held for the entire operation, so the
    /// closure may safely read disk state that the lock protects (e.g.
    /// `packages/<pkg>/graph.json`). It must NOT perform long-running
    /// or external I/O — keep it under a few milliseconds.
    ///
    /// Returns the post-mutation `Project` so the caller can hand it
    /// straight to the HTTP layer.
    pub async fn mutate_project<F>(
        &self,
        slug: &Slug,
        mutator: F,
    ) -> Result<Project, ApiError>
    where
        F: FnOnce(&mut Project) -> Result<(), ApiError>,
    {
        let dir = self.project_dir(slug);
        // Existence probe first so callers get NotFound instead of an
        // arbitrary I/O error if the project was deleted in the meantime.
        match fs::metadata(&dir).await {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(ApiError::NotFound);
            }
            Err(err) => return Err(err.into()),
        }

        let lock = self.lock_for(slug);
        let _guard = lock.lock().await;

        let project_path = dir.join(PROJECT_FILE);
        let mut project: Project = read_json(&project_path).await?;
        mutator(&mut project)?;
        project
            .validate_package_tree()
            .map_err(|e| ApiError::Conflict(format!("package tree invariant violated: {e}")))?;
        project.meta.updated_at = OffsetDateTime::now_utc();
        write_json_atomic(&project_path, &project).await?;
        Ok(project)
    }

    /// Persist a graph for a specific package within a project. Same
    /// validation pipeline as [`save_graph`] (schema version + per-node
    /// registry checks + structural validation), routed to
    /// `packages/<pkg-slug>/graph.json`.
    ///
    /// The caller must hold (or be implicitly serialised against) the
    /// project-level lock — this function takes the per-slug lock
    /// itself, so do not call it from inside a `mutate_project` closure.
    pub async fn save_graph_for_package(
        &self,
        slug: &Slug,
        pkg_slug: &PackageSlug,
        graph: &Graph,
        registry: &crate::templates::TemplateRegistry,
    ) -> Result<(), ApiError> {
        if graph.schema_version != GRAPH_SCHEMA_VERSION {
            return Err(ApiError::InvalidGraph(format!(
                "unsupported graph schema_version {}, expected {}",
                graph.schema_version, GRAPH_SCHEMA_VERSION
            )));
        }
        for node in &graph.nodes {
            if let Err(err) = registry.validate(&node.template_id, &node.config) {
                return Err(err.into());
            }
        }
        if let Err(errors) = crate::projects::validation::validate_graph(graph, registry) {
            let joined: Vec<String> = errors.iter().map(|e| e.message()).collect();
            return Err(ApiError::InvalidGraph(joined.join("; ")));
        }

        let dir = self.project_dir(slug);
        match fs::metadata(&dir).await {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(ApiError::NotFound);
            }
            Err(err) => return Err(err.into()),
        }

        let lock = self.lock_for(slug);
        let _guard = lock.lock().await;

        // Verify the package actually exists in the project tree before
        // touching disk — otherwise a typo'd package slug would create
        // an orphan folder on every save.
        let project: Project = read_json(&dir.join(PROJECT_FILE)).await?;
        if !project.packages.iter().any(|p| &p.slug == pkg_slug) {
            return Err(ApiError::NotFound);
        }

        let pkg_dir = self.package_dir(slug, pkg_slug);
        fs::create_dir_all(&pkg_dir).await?;
        write_json_atomic(&pkg_dir.join(GRAPH_FILE), graph).await?;
        Ok(())
    }

    /// Remove a package's on-disk folder (graph + any future per-package
    /// metadata). Idempotent: missing folder is treated as success.
    /// Caller must hold the per-slug lock and is responsible for
    /// updating `Project.packages` separately.
    pub(super) async fn delete_package_dir(
        &self,
        slug: &Slug,
        pkg_slug: &PackageSlug,
    ) -> Result<(), ApiError> {
        let pkg_dir = self.package_dir(slug, pkg_slug);
        match fs::remove_dir_all(&pkg_dir).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// Rename a package's on-disk folder. POSIX `rename` is atomic on
    /// the same filesystem. Caller holds the per-slug lock.
    pub(super) async fn rename_package_dir(
        &self,
        slug: &Slug,
        old: &PackageSlug,
        new: &PackageSlug,
    ) -> Result<(), ApiError> {
        let from = self.package_dir(slug, old);
        let to = self.package_dir(slug, new);
        // If the source doesn't exist (package was created but never saved
        // a graph), treat the rename as a no-op so the caller's tree
        // update still succeeds.
        if fs::metadata(&from).await.is_err() {
            return Ok(());
        }
        if fs::metadata(&to).await.is_ok() {
            return Err(ApiError::Conflict(format!(
                "destination package folder {} already exists on disk",
                new.as_str()
            )));
        }
        fs::rename(&from, &to).await?;
        Ok(())
    }

    /// Load the project's root-package flow graph.
    ///
    /// Backwards-compatible shim that targets the root package
    /// (`packages/main/graph.json`). The legacy single-graph layout is
    /// migrated on first call. T3 will introduce
    /// [`load_graph_for_package`] for per-package addressing.
    pub async fn load_graph(&self, slug: &Slug) -> Result<Graph, ApiError> {
        self.migrate_legacy_graph_if_needed(slug).await?;
        let path = self.package_graph_path(slug, &PackageSlug::root());
        read_json(&path).await
    }

    /// Load a specific package's flow graph. Used by T3+ HTTP handlers
    /// and by T4+ codegen to emit one module per package.
    pub async fn load_graph_for_package(
        &self,
        slug: &Slug,
        pkg_slug: &PackageSlug,
    ) -> Result<Graph, ApiError> {
        self.migrate_legacy_graph_if_needed(slug).await?;
        let path = self.package_graph_path(slug, pkg_slug);
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
            if let Err(err) = registry.validate(&node.template_id, &node.config) {
                tracing::error!("NODE VALIDATION FAILED on node '{}' ({}): {:?}", node.id.0, node.template_id.as_str(), err);
                return Err(err.into());
            }
        }

        // Validate graph structure — edges must reference real nodes and
        // declared ports, and type tags must be compatible.
        if let Err(errors) = crate::projects::validation::validate_graph(graph, registry) {
            let messages: Vec<String> = errors.iter().map(|e| e.message()).collect();
            let joined = messages.join("; ");
            tracing::error!("GRAPH STRUCTURE VALIDATION FAILED: {}", joined);
            return Err(ApiError::InvalidGraph(joined));
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

        // Route the graph to the root package's folder. Ensure the dir
        // exists (may not, on a brand-new project that never called
        // migration); `create_dir_all` is idempotent.
        self.migrate_legacy_graph_if_needed(slug).await?;
        let root_pkg_dir = self.package_dir(slug, &PackageSlug::root());
        fs::create_dir_all(&root_pkg_dir).await?;
        write_json_atomic(&root_pkg_dir.join(GRAPH_FILE), graph).await?;

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

    /// Load the list of installed marketplace packages for `slug`.
    /// Returns an empty vector if `marketplace.json` does not exist on disk.
    pub async fn load_marketplace(&self, slug: &Slug) -> Result<Vec<String>, ApiError> {
        let dir = self.project_dir(slug);
        let path = dir.join("marketplace.json");
        
        match fs::metadata(&path).await {
            Ok(_) => {
                let packages: Vec<String> = read_json(&path).await?;
                Ok(packages)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Ok(Vec::new())
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Save the list of installed marketplace packages for `slug`.
    pub async fn save_marketplace(&self, slug: &Slug, packages: &[String]) -> Result<(), ApiError> {
        let dir = self.project_dir(slug);
        
        // Ensure project directory exists
        match fs::metadata(&dir).await {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Err(ApiError::NotFound);
            }
            Err(err) => return Err(err.into()),
        }

        let lock = self.lock_for(slug);
        let _guard = lock.lock().await;

        let path = dir.join("marketplace.json");
        write_json_atomic(&path, &packages.to_vec()).await?;
        
        Ok(())
    }

    /// Export a project as a serialized in-memory `.flow` (ZIP) archive.
    ///
    /// The on-disk multi-package layout (T2) is flattened to a
    /// single-graph zip for now — the root package's graph is exported as
    /// `graph.json` inside the archive for forward-compat with v1
    /// importers. Section-1 T3 will extend this to export every package
    /// once user-driven multi-package CRUD lands.
    pub async fn export_archive(&self, slug: &Slug) -> Result<Vec<u8>, ApiError> {
        let dir = self.project_dir(slug);
        // Migrate first so a legacy project can be exported without
        // surfacing the legacy layout in the zip.
        self.migrate_legacy_graph_if_needed(slug).await?;

        let project_bytes = fs::read(dir.join(PROJECT_FILE)).await.map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                ApiError::NotFound
            } else {
                err.into()
            }
        })?;

        let graph_path = self.package_graph_path(slug, &PackageSlug::root());
        let graph_bytes = fs::read(&graph_path).await.map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                ApiError::NotFound
            } else {
                err.into()
            }
        })?;

        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let options = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);

            zip.start_file(PROJECT_FILE, options)
                .map_err(|e| ApiError::Internal(format!("Failed to create project.json in zip: {}", e)))?;
            std::io::Write::write_all(&mut zip, &project_bytes)
                .map_err(|e| ApiError::Internal(format!("Failed to write project.json in zip: {}", e)))?;

            zip.start_file(GRAPH_FILE, options)
                .map_err(|e| ApiError::Internal(format!("Failed to create graph.json in zip: {}", e)))?;
            std::io::Write::write_all(&mut zip, &graph_bytes)
                .map_err(|e| ApiError::Internal(format!("Failed to write graph.json in zip: {}", e)))?;

            zip.finish()
                .map_err(|e| ApiError::Internal(format!("Failed to finalize zip archive: {}", e)))?;
        }

        Ok(buf)
    }

    /// Import a project from a serialized `.flow` (ZIP) archive.
    /// Resolves slug collisions by appending a sequential suffix (e.g. -import, -1)
    /// and returns the newly saved (Project, Graph).
    pub async fn import_archive(&self, zip_bytes: &[u8]) -> Result<(Project, Graph), ApiError> {
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
            .map_err(|e| ApiError::InvalidBody(format!("Invalid zip archive: {}", e)))?;

        let mut project_str = String::new();
        {
            let mut project_entry = archive.by_name(PROJECT_FILE)
                .map_err(|_| ApiError::InvalidBody(format!("Missing {} inside flow archive", PROJECT_FILE)))?;
            std::io::Read::read_to_string(&mut project_entry, &mut project_str)
                .map_err(|e| ApiError::Internal(format!("Failed to read project.json from zip: {}", e)))?;
        }
        
        let mut project: Project = serde_json::from_str(&project_str)
            .map_err(|e| ApiError::InvalidBody(format!("Failed to parse project.json: {}", e)))?;

        let mut graph_str = String::new();
        {
            let mut graph_entry = archive.by_name(GRAPH_FILE)
                .map_err(|_| ApiError::InvalidBody(format!("Missing {} inside flow archive", GRAPH_FILE)))?;
            std::io::Read::read_to_string(&mut graph_entry, &mut graph_str)
                .map_err(|e| ApiError::Internal(format!("Failed to read graph.json from zip: {}", e)))?;
        }
        
        let graph: Graph = serde_json::from_str(&graph_str)
            .map_err(|e| ApiError::InvalidBody(format!("Failed to parse graph.json: {}", e)))?;

        // Resolve slug collision
        let original_slug = project.meta.slug.as_str().to_string();
        let mut current_slug_str = original_slug.clone();
        let mut suffix_counter = 0;

        loop {
            let candidate_slug = Slug::new(&current_slug_str);
            match candidate_slug {
                Ok(slug) => {
                    let dir = self.project_dir(&slug);
                    if !dir.exists() {
                        project.meta.slug = slug;
                        break;
                    }
                }
                Err(_) => {}
            }

            suffix_counter += 1;
            if suffix_counter == 1 {
                current_slug_str = format!("{}-import", original_slug);
            } else {
                current_slug_str = format!("{}-{}", original_slug, suffix_counter - 1);
            }
        }

        if suffix_counter > 0 {
            project.meta.name = format!("{} (Imported)", project.meta.name);
        }

        // T2: validate the incoming package tree before persisting. An
        // archive crafted with a malformed tree must not be importable.
        project
            .validate_package_tree()
            .map_err(|e| ApiError::InvalidBody(format!("project.json: {e}")))?;

        let new_dir = self.project_dir(&project.meta.slug);
        fs::create_dir_all(&new_dir).await?;
        // T2: write the imported graph under the root package layout, not
        // at the project root. Pre-Section-1 zip exports remain
        // importable because the zip entry is still named `graph.json`.
        let root_pkg_dir = self.package_dir(&project.meta.slug, &PackageSlug::root());
        fs::create_dir_all(&root_pkg_dir).await?;

        write_json_atomic(&new_dir.join(PROJECT_FILE), &project).await?;
        write_json_atomic(&root_pkg_dir.join(GRAPH_FILE), &graph).await?;

        Ok((project, graph))
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
            .create(slug("user-service"), "User service".into(), None)
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
        store.create(slug("dupe"), "first".into(), None).await.unwrap();
        let err = store.create(slug("dupe"), "second".into(), None).await.unwrap_err();
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
        store.create(slug("doomed"), "x".into(), None).await.unwrap();
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

        store.create(slug("alpha"), "Alpha".into(), None).await.unwrap();
        // Sleep a microsecond so created_at strictly orders.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        store.create(slug("beta"), "Beta".into(), None).await.unwrap();

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
            .create(slug("flow"), "Flow".into(), None)
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
        store.create(slug("ver"), "v".into(), None).await.unwrap();
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
        store.create(slug("badt"), "x".into(), None).await.unwrap();
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
        store.create(slug("badc"), "x".into(), None).await.unwrap();
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
        store.create(slug("hot"), "hot".into(), None).await.unwrap();

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

    #[tokio::test]
    async fn test_flow_export_and_import_roundtrip() {
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        
        let slug_name = slug("export-test");
        store.create(slug_name.clone(), "Export Test".into(), None).await.unwrap();

        // Save some graph nodes
        let mut g = Graph::default();
        g.nodes.push(crate::projects::types::Node {
            id: crate::projects::types::NodeId("node_1".into()),
            template_id: crate::templates::TemplateId::new("core.entry_point").unwrap(),
            position: crate::projects::types::Position { x: 100.0, y: 100.0 },
            config: serde_json::json!({}),
            label: Some("Start".into()),
            comment: Some("Initial Entry Point".into()),
        });
        
        let reg = registry();
        store.save_graph(&slug_name, &g, &reg).await.unwrap();

        // Export archive
        let zip_bytes = store.export_archive(&slug_name).await.unwrap();
        assert!(!zip_bytes.is_empty(), "Exported zip should not be empty");

        // Import it in another fresh store
        let dir2 = tempdir().unwrap();
        let store2 = ProjectStore::new(dir2.path()).await.unwrap();
        let (imported_proj, imported_graph) = store2.import_archive(&zip_bytes).await.unwrap();

        assert_eq!(imported_proj.meta.slug.as_str(), "export-test");
        assert_eq!(imported_proj.meta.name, "Export Test");
        assert_eq!(imported_graph.nodes.len(), 1);
        assert_eq!(imported_graph.nodes[0].id.0, "node_1");
        assert_eq!(imported_graph.nodes[0].comment.as_deref(), Some("Initial Entry Point"));
    }

    #[tokio::test]
    async fn test_flow_import_slug_collision_resolution() {
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        
        let slug_name = slug("collision");
        store.create(slug_name.clone(), "Original".into(), None).await.unwrap();

        // Export a flow package of the first project
        let zip_bytes = store.export_archive(&slug_name).await.unwrap();

        // Import it back into the same store! This should collide with "collision"
        let (imported_proj, _) = store.import_archive(&zip_bytes).await.unwrap();

        // First collision appends "-import"
        assert_eq!(imported_proj.meta.slug.as_str(), "collision-import");
        assert_eq!(imported_proj.meta.name, "Original (Imported)");

        // Import it again! This should collide with both "collision" and "collision-import", appending "-1"
        let (imported_proj2, _) = store.import_archive(&zip_bytes).await.unwrap();
        assert_eq!(imported_proj2.meta.slug.as_str(), "collision-1");
        assert_eq!(imported_proj2.meta.name, "Original (Imported)");
    }

    // ----- T2: per-package graph layout + legacy migration -----

    #[tokio::test]
    async fn test_create_writes_graph_under_packages_main_not_root() {
        // New projects must use the post-Section-1 disk layout from the
        // very first save — otherwise the legacy migration path would
        // need to run on every brand-new project, which is wasteful and
        // hides bugs in `create`.
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        let s = slug("new-layout");
        store.create(s.clone(), "n".into(), None).await.unwrap();

        let root = dir.path().join("new-layout");
        assert!(
            !root.join("graph.json").exists(),
            "post-T2 creates must not write the legacy top-level graph.json"
        );
        assert!(
            root.join("packages").join("main").join("graph.json").exists(),
            "graph must live at packages/main/graph.json"
        );
    }

    #[tokio::test]
    async fn test_load_migrates_legacy_top_level_graph_into_root_package() {
        // Simulate a pre-Section-1 project on disk: project.json without
        // a `packages` field, graph.json at the project root. The first
        // `load` must hoist the graph into `packages/main/graph.json`
        // and leave the file readable through the standard `load_graph`
        // shim.
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        let project_root = dir.path().join("legacy-proj");
        fs::create_dir(&project_root).await.unwrap();

        // Hand-craft the legacy on-disk shape — no `packages` field, no
        // `packages/` dir.
        let legacy_project_json = serde_json::json!({
            "slug": "legacy-proj",
            "name": "Legacy",
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:00:00Z",
            "schema_version": PROJECT_SCHEMA_VERSION
        });
        fs::write(
            project_root.join(PROJECT_FILE),
            serde_json::to_vec_pretty(&legacy_project_json).unwrap(),
        )
        .await
        .unwrap();

        let legacy_graph = Graph::default();
        fs::write(
            project_root.join(GRAPH_FILE),
            serde_json::to_vec_pretty(&legacy_graph).unwrap(),
        )
        .await
        .unwrap();

        // First load triggers migration.
        let s = slug("legacy-proj");
        let p = store.load(&s).await.unwrap();
        assert_eq!(p.packages.len(), 1, "legacy project must synthesise one root package");
        assert_eq!(p.packages[0].slug.as_str(), "main");

        // Disk state: legacy file gone, new file present.
        assert!(
            !project_root.join("graph.json").exists(),
            "migration must remove (rename) the legacy graph.json"
        );
        assert!(
            project_root.join("packages").join("main").join("graph.json").exists(),
            "migration must place the graph under packages/main/"
        );

        // The shim still works.
        let g = store.load_graph(&s).await.unwrap();
        assert_eq!(g.schema_version, GRAPH_SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn test_migration_is_idempotent_across_repeated_loads() {
        // After the first migration runs, subsequent loads must not
        // re-trigger it (it's already done) and must not error.
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        let s = slug("idem");
        store.create(s.clone(), "i".into(), None).await.unwrap();

        // Three back-to-back loads — none should fail or alter state.
        for _ in 0..3 {
            let _ = store.load(&s).await.unwrap();
            let _ = store.load_graph(&s).await.unwrap();
        }
        // Layout unchanged.
        let root = dir.path().join("idem");
        assert!(!root.join("graph.json").exists());
        assert!(root.join("packages").join("main").join("graph.json").exists());
    }

    #[tokio::test]
    async fn test_load_rejects_malformed_package_tree_with_invalid_body() {
        // A project.json that decodes structurally but violates a tree
        // invariant (here: two roots) must surface as InvalidBody so the
        // handler returns 422, not a 500 deep inside codegen.
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        let project_root = dir.path().join("bad-tree");
        fs::create_dir(&project_root).await.unwrap();

        let bad = serde_json::json!({
            "slug": "bad-tree",
            "name": "Bad",
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:00:00Z",
            "schema_version": PROJECT_SCHEMA_VERSION,
            "packages": [
                { "id": "a", "slug": "alpha", "parent_id": null },
                { "id": "b", "slug": "beta", "parent_id": null }
            ]
        });
        fs::write(
            project_root.join(PROJECT_FILE),
            serde_json::to_vec(&bad).unwrap(),
        )
        .await
        .unwrap();

        let err = store.load(&slug("bad-tree")).await.unwrap_err();
        match err {
            ApiError::InvalidBody(msg) => {
                assert!(
                    msg.contains("project.json") && msg.contains("root"),
                    "InvalidBody message must mention the file and the rule violation; got: {msg}"
                );
            }
            other => panic!("expected InvalidBody, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_concurrent_legacy_load_tolerates_rename_race() {
        // Two concurrent `load` calls on a freshly-staged legacy project
        // both observe the legacy file before either rename completes.
        // The loser of the rename race sees `NotFound` from `fs::rename`;
        // the migration helper must treat that as "already done" rather
        // than propagate it as a 500. Verified by spawning N parallel
        // loads — every one must succeed, and the post-condition (graph
        // at the new path, legacy file gone) must hold.
        let dir = tempdir().unwrap();
        let store = Arc::new(ProjectStore::new(dir.path()).await.unwrap());
        let project_root = dir.path().join("race-proj");
        fs::create_dir(&project_root).await.unwrap();

        let legacy_project_json = serde_json::json!({
            "slug": "race-proj",
            "name": "Race",
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:00:00Z",
            "schema_version": PROJECT_SCHEMA_VERSION
        });
        fs::write(
            project_root.join(PROJECT_FILE),
            serde_json::to_vec(&legacy_project_json).unwrap(),
        )
        .await
        .unwrap();
        fs::write(
            project_root.join(GRAPH_FILE),
            serde_json::to_vec(&Graph::default()).unwrap(),
        )
        .await
        .unwrap();

        let mut tasks = Vec::new();
        for _ in 0..8 {
            let store = store.clone();
            tasks.push(tokio::spawn(async move {
                store.load(&Slug::new("race-proj").unwrap()).await
            }));
        }
        for t in tasks {
            t.await.unwrap().expect("concurrent legacy load must not error");
        }

        assert!(!project_root.join("graph.json").exists());
        assert!(project_root.join("packages").join("main").join("graph.json").exists());
    }

    #[tokio::test]
    async fn test_save_graph_creates_root_package_dir_if_missing() {
        // Safety net: even if the migration helper somehow didn't run
        // (e.g. a project with no graph yet whose first I/O is a save),
        // save_graph must still succeed by lazily creating packages/main/.
        let dir = tempdir().unwrap();
        let store = ProjectStore::new(dir.path()).await.unwrap();
        let s = slug("lazy-dir");

        // Build the minimum on-disk state: just project.json with the
        // canonical post-T1 shape. No graph file anywhere, no
        // packages/ dir.
        let project_root = dir.path().join("lazy-dir");
        fs::create_dir(&project_root).await.unwrap();
        let proj = serde_json::json!({
            "slug": "lazy-dir",
            "name": "Lazy",
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:00:00Z",
            "schema_version": PROJECT_SCHEMA_VERSION,
            "packages": [
                { "id": "pkg-root", "slug": "main", "parent_id": null }
            ]
        });
        fs::write(
            project_root.join(PROJECT_FILE),
            serde_json::to_vec(&proj).unwrap(),
        )
        .await
        .unwrap();

        // Save a fresh graph; must succeed and produce the expected
        // disk layout without any prior load call.
        let reg = registry();
        let g = Graph::default();
        store.save_graph(&s, &g, &reg).await.unwrap();
        assert!(project_root.join("packages").join("main").join("graph.json").exists());
    }
}
