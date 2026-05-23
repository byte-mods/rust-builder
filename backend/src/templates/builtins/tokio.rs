//! Visual-Rust Tokio runtime nodes (S15b foundation).
//!
//! Hosts node templates that emit Tokio runtime *primitives* as **top-level
//! constructor helpers** in the generated user-project. Each template is a
//! definitional emitter — like [`super::language::LanguageFn`] — that
//! produces a `pub fn make_<name>() -> ...` (or `spawn_<name>()`) into
//! `src/runtime/<snake>.rs`.
//!
//! ## Why constructor helpers (and not inline snippets)
//!
//! V1 deliberately avoids in-body / statement-level emission. That would
//! require the scope-tracking work deferred to S15e (each emission site
//! would need to know which `async fn` body it lands in, validate context,
//! and reserve a body-insertion region). Top-level helpers sidestep all of
//! that: the studio emits a callable, the user wires it up in the parent
//! graph nodes, and `cargo check` catches mis-wired async/sync context
//! down the line with a normal compiler error.
//!
//! Consistency with S15a is the second reason: every visual-Rust node so
//! far emits a top-level item. Tokio nodes follow the same shape, so the
//! mental model (`<node>` → `<top-level item>`) is uniform.
//!
//! ## Codegen invariants
//!
//! - Names are **snake_case** Rust identifiers (these become function
//!   names, not type names), validated at schema time via
//!   [`super::language::is_valid_snake_case_ident`] plus the load-bearing
//!   `syn::parse_str::<syn::Ident>` reserved-word guard inherited from
//!   S15a.
//! - Item / payload types are **parseable `syn::Type`**, validated at
//!   schema time so garbage strings (`"Vec<{"`) fail fast.
//! - Output runs through `prettyplease::unparse` — byte-stable across
//!   regen calls.
//! - No dependency hints are emitted: `tokio` is already in
//!   `baseline_dependencies` with the `macros`, `rt-multi-thread`,
//!   `signal`, `sync`, `time` features that cover every primitive in
//!   this batch.

use proc_macro2::TokenStream;
use quote::{format_ident, quote, ToTokens};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use syn::parse_str;

use crate::templates::{
    codegen::{CodegenCtx, EmittedItem, RuntimeEmission, SchemaEmission},
    ports::PortSpec,
    CodegenMode, DebugBridgeKind, NodeTemplate, TemplateDisplay, TemplateError, TemplateId,
};

use super::language::{format_tokens, is_valid_snake_case_ident, Visibility};

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

/// Parameter configuration for spawned or concurrent task templates.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct SpawnParam {
    pub(super) name: String,
    pub(super) ty: String,
}

/// Validate a user-supplied helper-function name. snake_case + reserved-word
/// guard, mirroring the load-bearing pattern from S15a — `format_ident!`
/// panics on Rust reserved words, so the `syn::parse_str::<syn::Ident>`
/// check must run before any identifier reaches `quote!`.
fn validate_helper_name(template_id: &str, field: &str, name: &str) -> Result<(), TemplateError> {
    if !is_valid_snake_case_ident(name) {
        return Err(TemplateError::ConfigMismatch(format!(
            "{template_id}: {field} {name:?} is not a snake_case Rust identifier"
        )));
    }
    if parse_str::<syn::Ident>(name).is_err() {
        return Err(TemplateError::ConfigMismatch(format!(
            "{template_id}: {field} {name:?} is a Rust reserved word or otherwise not a valid identifier"
        )));
    }
    Ok(())
}

/// Validate a user-supplied type literal — must parse as a `syn::Type`.
fn validate_type(template_id: &str, field: &str, ty: &str) -> Result<(), TemplateError> {
    if parse_str::<syn::Type>(ty).is_err() {
        return Err(TemplateError::ConfigMismatch(format!(
            "{template_id}: {field} {ty:?} is not a valid Rust type"
        )));
    }
    Ok(())
}

/// Validate a user-supplied expression literal — must parse as a `syn::Expr`.
fn validate_expression(template_id: &str, field: &str, expr: &str) -> Result<(), TemplateError> {
    if parse_str::<syn::Expr>(expr).is_err() {
        return Err(TemplateError::ConfigMismatch(format!(
            "{template_id}: {field} {expr:?} is not a valid Rust expression"
        )));
    }
    Ok(())
}

/// Parse a pre-validated type literal into tokens — `validate()` has
/// already ensured it parses, so this `expect` is unreachable on any path
/// that went through validation.
fn type_tokens(template_id: &str, ty_str: &str) -> TokenStream {
    let ty: syn::Type = parse_str(ty_str)
        .unwrap_or_else(|e| panic!("{template_id}: type parsed in validate() but failed here: {e}"));
    ty.into_token_stream()
}

// ===========================================================================
// tokio.mpsc
// ===========================================================================

/// Per-instance config for [`TokioMpsc`].
///
/// Emits a constructor helper that returns a `(Sender<T>, Receiver<T>)`
/// pair for a multi-producer / single-consumer channel.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct TokioMpscConfig {
    /// snake_case identifier — becomes the helper fn name: `make_<name>`,
    /// and the emission file path: `src/runtime/<name>.rs`.
    pub(super) name: String,
    /// Item type carried on the channel — any `syn::parse_str::<syn::Type>`
    /// accepts. Examples: `u64`, `String`, `crate::types::Order`.
    pub(super) item_type: String,
    /// Channel buffer capacity. Must be `>= 1` — `tokio::sync::mpsc::channel(0)`
    /// panics at runtime, and the studio's "no panic on a reachable path"
    /// rule forbids letting that reach the generated binary.
    pub(super) buffer: usize,
    /// Visibility on the emitted helper fn. Defaults to `pub`.
    #[serde(default)]
    pub(super) visibility: Visibility,
}

impl TokioMpscConfig {
    fn validate(&self) -> Result<(), TemplateError> {
        validate_helper_name("tokio.mpsc", "name", &self.name)?;
        validate_type("tokio.mpsc", "item_type", &self.item_type)?;
        if self.buffer == 0 {
            // mpsc::channel(0) panics at runtime — reject at schema time
            // so this never reaches a built binary.
            return Err(TemplateError::ConfigMismatch(
                "tokio.mpsc: buffer must be >= 1 (mpsc::channel(0) panics at runtime)".into(),
            ));
        }
        Ok(())
    }
}

/// `tokio.mpsc` — emits a typed multi-producer / single-consumer channel
/// constructor.
///
/// Emission shape (example, `name=orders`, `item_type=Order`, `buffer=128`):
/// ```ignore
/// pub fn make_orders() -> (
///     tokio::sync::mpsc::Sender<Order>,
///     tokio::sync::mpsc::Receiver<Order>,
/// ) {
///     tokio::sync::mpsc::channel(128usize)
/// }
/// ```
///
/// File: `src/runtime/<name>.rs`. The shared `runtime/` directory keeps
/// every Tokio primitive in one place; the codegen orchestrator (see
/// `Generator::generate_project`) auto-creates `mod.rs` and the `mod
/// runtime;` declaration in `lib.rs`.
pub struct TokioMpsc {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl TokioMpsc {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("tokio.mpsc").expect("tokio.mpsc id is static and valid"),
            display: TemplateDisplay::new(
                "Tokio MPSC channel",
                "Tokio",
                "Multi-producer / single-consumer channel. Emits a constructor helper returning (Sender<T>, Receiver<T>) into src/runtime/.",
            ),
            // Like other definitional templates, no runtime ports.
            inputs: vec![],
            outputs: vec![],
            schema: serde_json::to_value(schemars::schema_for!(TokioMpscConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for TokioMpsc {
    fn id(&self) -> &TemplateId {
        &self.id
    }
    fn display(&self) -> &TemplateDisplay {
        &self.display
    }
    fn input_ports(&self) -> &[PortSpec] {
        &self.inputs
    }
    fn output_ports(&self) -> &[PortSpec] {
        &self.outputs
    }
    fn config_schema(&self) -> &Value {
        &self.schema
    }
    /// Constructor helper is a *runtime artifact* (it runs when the user's
    /// graph code calls it), not a type definition — so it routes through
    /// `emit_runtime`, mirroring `language.fn`. The orchestrator drops the
    /// file into `src/runtime/` and wires up `mod runtime;` automatically.
    fn codegen_mode(&self) -> CodegenMode {
        CodegenMode::Runtime
    }
    /// PassThrough: the helper itself does no async work that needs
    /// instrumentation — it returns the constructed primitive. Downstream
    /// usage of the channel is what gets debug-bridged, by other nodes.
    fn debug_bridge(&self) -> DebugBridgeKind {
        DebugBridgeKind::PassThrough
    }

    fn emit_schema(&self, _ctx: &CodegenCtx<'_>) -> Result<SchemaEmission, TemplateError> {
        Ok(SchemaEmission {
            items: vec![],
            dependencies: vec![],
        })
    }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: TokioMpscConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;
        config.validate()?;

        let fn_name = format_ident!("make_{}", config.name);
        let vis_tokens = config.visibility.as_tokens();
        let item_ty = type_tokens("tokio.mpsc", &config.item_type);
        let buffer = config.buffer;

        // `usize`-suffixed literal so the emission is unambiguous and
        // byte-stable across regen (no inference-context dependency).
        let tokens = quote! {
            #vis_tokens fn #fn_name() -> (
                tokio::sync::mpsc::Sender<#item_ty>,
                tokio::sync::mpsc::Receiver<#item_ty>,
            ) {
                tokio::sync::mpsc::channel(#buffer)
            }
        };

        let formatted = format_tokens("tokio.mpsc", tokens)?;

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("runtime/{}.rs", config.name),
                source: formatted,
            }],
            // `tokio` ships in baseline_dependencies with the `sync`
            // feature — no extra hint needed.
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ===========================================================================
// tokio.broadcast
// ===========================================================================

/// Per-instance config for [`TokioBroadcast`].
///
/// Emits a constructor helper that returns a `(Sender<T>, Receiver<T>)`
/// pair for a fan-out broadcast channel.
#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct TokioBroadcastConfig {
    /// snake_case identifier — becomes the helper fn name (`make_<name>`)
    /// and the emission path (`src/runtime/<name>.rs`).
    pub(super) name: String,
    /// Item type carried on the channel. Note: `broadcast::Sender<T>` requires
    /// `T: Clone` at runtime — the user's responsibility, surfaced by
    /// `cargo check` rather than enforced at codegen time.
    pub(super) item_type: String,
    /// Channel capacity. Must be `>= 1` — `broadcast::channel(0)` panics
    /// at runtime (via internal assertion). Capacity is the per-receiver
    /// ring-buffer size; older messages drop on overflow.
    pub(super) capacity: usize,
    /// Visibility on the emitted helper fn. Defaults to `pub`.
    #[serde(default)]
    pub(super) visibility: Visibility,
}

impl TokioBroadcastConfig {
    fn validate(&self) -> Result<(), TemplateError> {
        validate_helper_name("tokio.broadcast", "name", &self.name)?;
        validate_type("tokio.broadcast", "item_type", &self.item_type)?;
        if self.capacity == 0 {
            // broadcast::channel(0) panics at runtime — reject early.
            return Err(TemplateError::ConfigMismatch(
                "tokio.broadcast: capacity must be >= 1 (broadcast::channel(0) panics at runtime)".into(),
            ));
        }
        Ok(())
    }
}

/// `tokio.broadcast` — emits a typed fan-out broadcast channel constructor.
///
/// Emission shape (example, `name=events`, `item_type=Event`, `capacity=64`):
/// ```ignore
/// pub fn make_events() -> (
///     tokio::sync::broadcast::Sender<Event>,
///     tokio::sync::broadcast::Receiver<Event>,
/// ) {
///     tokio::sync::broadcast::channel(64usize)
/// }
/// ```
///
/// File: `src/runtime/<name>.rs`. Shares the `runtime/` directory with the
/// other Tokio primitives. `T: Clone` is not enforced at codegen — broadcast
/// requires it but the constraint surfaces cleanly via `cargo check`.
pub struct TokioBroadcast {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl TokioBroadcast {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("tokio.broadcast")
                .expect("tokio.broadcast id is static and valid"),
            display: TemplateDisplay::new(
                "Tokio broadcast channel",
                "Tokio",
                "Fan-out broadcast channel. Emits a constructor helper returning (Sender<T>, Receiver<T>) into src/runtime/. T must be Clone at runtime.",
            ),
            inputs: vec![],
            outputs: vec![],
            schema: serde_json::to_value(schemars::schema_for!(TokioBroadcastConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for TokioBroadcast {
    fn id(&self) -> &TemplateId {
        &self.id
    }
    fn display(&self) -> &TemplateDisplay {
        &self.display
    }
    fn input_ports(&self) -> &[PortSpec] {
        &self.inputs
    }
    fn output_ports(&self) -> &[PortSpec] {
        &self.outputs
    }
    fn config_schema(&self) -> &Value {
        &self.schema
    }
    /// Runtime artifact (a constructor fn that runs at request time) —
    /// matches `tokio.mpsc` and `language.fn`.
    fn codegen_mode(&self) -> CodegenMode {
        CodegenMode::Runtime
    }
    fn debug_bridge(&self) -> DebugBridgeKind {
        DebugBridgeKind::PassThrough
    }

    fn emit_schema(&self, _ctx: &CodegenCtx<'_>) -> Result<SchemaEmission, TemplateError> {
        Ok(SchemaEmission {
            items: vec![],
            dependencies: vec![],
        })
    }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: TokioBroadcastConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;
        config.validate()?;

        let fn_name = format_ident!("make_{}", config.name);
        let vis_tokens = config.visibility.as_tokens();
        let item_ty = type_tokens("tokio.broadcast", &config.item_type);
        let capacity = config.capacity;

        let tokens = quote! {
            #vis_tokens fn #fn_name() -> (
                tokio::sync::broadcast::Sender<#item_ty>,
                tokio::sync::broadcast::Receiver<#item_ty>,
            ) {
                tokio::sync::broadcast::channel(#capacity)
            }
        };

        let formatted = format_tokens("tokio.broadcast", tokens)?;

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("runtime/{}.rs", config.name),
                source: formatted,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

fn validate_param(template_id: &str, param: &SpawnParam) -> Result<(), TemplateError> {
    validate_helper_name(template_id, "param.name", &param.name)?;
    validate_type(template_id, "param.ty", &param.ty)?;
    Ok(())
}

// ===========================================================================
// tokio.mutex
// ===========================================================================

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct TokioMutexConfig {
    pub(super) name: String,
    pub(super) item_type: String,
    pub(super) initial_value: Option<String>,
    #[serde(default)]
    pub(super) visibility: Visibility,
}

impl TokioMutexConfig {
    fn validate(&self) -> Result<(), TemplateError> {
        validate_helper_name("tokio.mutex", "name", &self.name)?;
        validate_type("tokio.mutex", "item_type", &self.item_type)?;
        if let Some(ref val) = self.initial_value {
            validate_expression("tokio.mutex", "initial_value", val)?;
        }
        Ok(())
    }
}

pub struct TokioMutex {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl TokioMutex {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("tokio.mutex").expect("tokio.mutex id is static and valid"),
            display: TemplateDisplay::new(
                "Tokio Mutex",
                "Tokio",
                "Async Mutex lock. Emits a constructor helper returning tokio::sync::Mutex<T> into src/runtime/.",
            ),
            inputs: vec![],
            outputs: vec![],
            schema: serde_json::to_value(schemars::schema_for!(TokioMutexConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for TokioMutex {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::PassThrough }

    fn emit_schema(&self, _ctx: &CodegenCtx<'_>) -> Result<SchemaEmission, TemplateError> {
        Ok(SchemaEmission {
            items: vec![],
            dependencies: vec![],
        })
    }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: TokioMutexConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;
        config.validate()?;

        let fn_name = format_ident!("make_{}", config.name);
        let vis_tokens = config.visibility.as_tokens();
        let item_ty = type_tokens("tokio.mutex", &config.item_type);
        
        let init_expr = if let Some(ref val) = config.initial_value {
            let expr: syn::Expr = parse_str(val).unwrap();
            quote! { #expr }
        } else {
            quote! { Default::default() }
        };

        let tokens = quote! {
            #vis_tokens fn #fn_name() -> tokio::sync::Mutex<#item_ty> {
                tokio::sync::Mutex::new(#init_expr)
            }
        };

        let formatted = format_tokens("tokio.mutex", tokens)?;

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("runtime/{}.rs", config.name),
                source: formatted,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ===========================================================================
// tokio.rwlock
// ===========================================================================

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct TokioRwLockConfig {
    pub(super) name: String,
    pub(super) item_type: String,
    pub(super) initial_value: Option<String>,
    #[serde(default)]
    pub(super) visibility: Visibility,
}

impl TokioRwLockConfig {
    fn validate(&self) -> Result<(), TemplateError> {
        validate_helper_name("tokio.rwlock", "name", &self.name)?;
        validate_type("tokio.rwlock", "item_type", &self.item_type)?;
        if let Some(ref val) = self.initial_value {
            validate_expression("tokio.rwlock", "initial_value", val)?;
        }
        Ok(())
    }
}

pub struct TokioRwLock {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl TokioRwLock {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("tokio.rwlock").expect("tokio.rwlock id is static and valid"),
            display: TemplateDisplay::new(
                "Tokio RwLock",
                "Tokio",
                "Async RwLock lock. Emits a constructor helper returning tokio::sync::RwLock<T> into src/runtime/.",
            ),
            inputs: vec![],
            outputs: vec![],
            schema: serde_json::to_value(schemars::schema_for!(TokioRwLockConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for TokioRwLock {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::PassThrough }

    fn emit_schema(&self, _ctx: &CodegenCtx<'_>) -> Result<SchemaEmission, TemplateError> {
        Ok(SchemaEmission {
            items: vec![],
            dependencies: vec![],
        })
    }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: TokioRwLockConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;
        config.validate()?;

        let fn_name = format_ident!("make_{}", config.name);
        let vis_tokens = config.visibility.as_tokens();
        let item_ty = type_tokens("tokio.rwlock", &config.item_type);
        
        let init_expr = if let Some(ref val) = config.initial_value {
            let expr: syn::Expr = parse_str(val).unwrap();
            quote! { #expr }
        } else {
            quote! { Default::default() }
        };

        let tokens = quote! {
            #vis_tokens fn #fn_name() -> tokio::sync::RwLock<#item_ty> {
                tokio::sync::RwLock::new(#init_expr)
            }
        };

        let formatted = format_tokens("tokio.rwlock", tokens)?;

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("runtime/{}.rs", config.name),
                source: formatted,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ===========================================================================
// tokio.spawn
// ===========================================================================

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct TokioSpawnConfig {
    pub(super) name: String,
    #[serde(default)]
    pub(super) params: Vec<SpawnParam>,
    pub(super) body: String,
    #[serde(default)]
    pub(super) visibility: Visibility,
}

impl TokioSpawnConfig {
    fn validate(&self) -> Result<(), TemplateError> {
        validate_helper_name("tokio.spawn", "name", &self.name)?;
        for param in &self.params {
            validate_param("tokio.spawn", param)?;
        }
        let check_expr = format!("async move {{ {} }}", self.body);
        if parse_str::<syn::Expr>(&check_expr).is_err() {
            return Err(TemplateError::ConfigMismatch(format!(
                "tokio.spawn: body contains invalid Rust code: {:?}",
                self.body
            )));
        }
        Ok(())
    }
}

pub struct TokioSpawn {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl TokioSpawn {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("tokio.spawn").expect("tokio.spawn id is static and valid"),
            display: TemplateDisplay::new(
                "Tokio Spawn Task",
                "Tokio",
                "Spawns an asynchronous background task. Emits a spawn helper into src/runtime/.",
            ),
            inputs: vec![],
            outputs: vec![],
            schema: serde_json::to_value(schemars::schema_for!(TokioSpawnConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for TokioSpawn {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::PassThrough }

    fn emit_schema(&self, _ctx: &CodegenCtx<'_>) -> Result<SchemaEmission, TemplateError> {
        Ok(SchemaEmission {
            items: vec![],
            dependencies: vec![],
        })
    }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: TokioSpawnConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;
        config.validate()?;

        let fn_name = format!("spawn_{}", config.name);
        let vis_str = config.visibility.as_tokens().to_string();

        let mut params_vec = Vec::new();
        let mut clones_vec = Vec::new();

        for param in &config.params {
            params_vec.push(format!("{}: {}", param.name, param.ty));
            if config.body.contains(&param.name) {
                clones_vec.push(format!("let {} = /*[clone:{}]*/{};", param.name, param.name, param.name));
            }
        }

        let params_str = params_vec.join(", ");
        let clones_str = if clones_vec.is_empty() {
            String::new()
        } else {
            format!("{}\n    ", clones_vec.join("\n    "))
        };

        let source = format!(
            "{} fn {}({}) -> tokio::task::JoinHandle<()> {{\n    {}tokio::spawn(async move {{\n        {}\n    }})\n}}\n",
            vis_str, fn_name, params_str, clones_str, config.body
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("runtime/{}.rs", config.name),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ===========================================================================
// tokio.sleep
// ===========================================================================

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct TokioSleepConfig {
    pub(super) name: String,
    pub(super) duration_ms: u64,
    #[serde(default)]
    pub(super) visibility: Visibility,
}

impl TokioSleepConfig {
    fn validate(&self) -> Result<(), TemplateError> {
        validate_helper_name("tokio.sleep", "name", &self.name)?;
        Ok(())
    }
}

pub struct TokioSleep {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl TokioSleep {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("tokio.sleep").expect("tokio.sleep id is static and valid"),
            display: TemplateDisplay::new(
                "Tokio Sleep",
                "Tokio",
                "Asynchronously sleeps for a duration. Emits a sleep helper into src/runtime/.",
            ),
            inputs: vec![],
            outputs: vec![],
            schema: serde_json::to_value(schemars::schema_for!(TokioSleepConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for TokioSleep {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::PassThrough }

    fn emit_schema(&self, _ctx: &CodegenCtx<'_>) -> Result<SchemaEmission, TemplateError> {
        Ok(SchemaEmission {
            items: vec![],
            dependencies: vec![],
        })
    }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: TokioSleepConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;
        config.validate()?;

        let fn_name = format_ident!("sleep_{}", config.name);
        let vis_tokens = config.visibility.as_tokens();
        let ms = config.duration_ms;

        let tokens = quote! {
            #vis_tokens async fn #fn_name() {
                tokio::time::sleep(tokio::time::Duration::from_millis(#ms)).await;
            }
        };

        let formatted = format_tokens("tokio.sleep", tokens)?;

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("runtime/{}.rs", config.name),
                source: formatted,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ===========================================================================
// tokio.interval
// ===========================================================================

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct TokioIntervalConfig {
    pub(super) name: String,
    pub(super) period_ms: u64,
    #[serde(default)]
    pub(super) visibility: Visibility,
}

impl TokioIntervalConfig {
    fn validate(&self) -> Result<(), TemplateError> {
        validate_helper_name("tokio.interval", "name", &self.name)?;
        if self.period_ms == 0 {
            return Err(TemplateError::ConfigMismatch(
                "tokio.interval: period must be >= 1 ms".into()
            ));
        }
        Ok(())
    }
}

pub struct TokioInterval {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl TokioInterval {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("tokio.interval").expect("tokio.interval id is static and valid"),
            display: TemplateDisplay::new(
                "Tokio Interval",
                "Tokio",
                "Asynchronous interval timer. Emits a constructor helper returning tokio::time::Interval into src/runtime/.",
            ),
            inputs: vec![],
            outputs: vec![],
            schema: serde_json::to_value(schemars::schema_for!(TokioIntervalConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for TokioInterval {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::PassThrough }

    fn emit_schema(&self, _ctx: &CodegenCtx<'_>) -> Result<SchemaEmission, TemplateError> {
        Ok(SchemaEmission {
            items: vec![],
            dependencies: vec![],
        })
    }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: TokioIntervalConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;
        config.validate()?;

        let fn_name = format_ident!("make_{}", config.name);
        let vis_tokens = config.visibility.as_tokens();
        let ms = config.period_ms;

        let tokens = quote! {
            #vis_tokens fn #fn_name() -> tokio::time::Interval {
                tokio::time::interval(tokio::time::Duration::from_millis(#ms))
            }
        };

        let formatted = format_tokens("tokio.interval", tokens)?;

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("runtime/{}.rs", config.name),
                source: formatted,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ===========================================================================
// tokio.select
// ===========================================================================

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct SelectBranch {
    pub(super) pattern: String,
    pub(super) body: String,
}

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct TokioSelectConfig {
    pub(super) name: String,
    #[serde(default)]
    pub(super) params: Vec<SpawnParam>,
    pub(super) branches: Vec<SelectBranch>,
    #[serde(default)]
    pub(super) visibility: Visibility,
}

impl TokioSelectConfig {
    fn validate(&self) -> Result<(), TemplateError> {
        validate_helper_name("tokio.select", "name", &self.name)?;
        for param in &self.params {
            validate_param("tokio.select", param)?;
        }
        if self.branches.is_empty() {
            return Err(TemplateError::ConfigMismatch(
                "tokio.select: must configure at least one select branch".into()
            ));
        }
        for (i, branch) in self.branches.iter().enumerate() {
            let check_branch = format!("async move {{ tokio::select! {{ {} => {{ {} }} }} }}", branch.pattern, branch.body);
            if parse_str::<syn::Expr>(&check_branch).is_err() {
                return Err(TemplateError::ConfigMismatch(format!(
                    "tokio.select: branch {} contains invalid Rust syntax: {} => {}",
                    i, branch.pattern, branch.body
                )));
            }
        }
        Ok(())
    }
}

pub struct TokioSelect {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl TokioSelect {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("tokio.select").expect("tokio.select id is static and valid"),
            display: TemplateDisplay::new(
                "Tokio Select",
                "Tokio",
                "Waits on multiple concurrent branches. Emits a select helper into src/runtime/.",
            ),
            inputs: vec![],
            outputs: vec![],
            schema: serde_json::to_value(schemars::schema_for!(TokioSelectConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for TokioSelect {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::PassThrough }

    fn emit_schema(&self, _ctx: &CodegenCtx<'_>) -> Result<SchemaEmission, TemplateError> {
        Ok(SchemaEmission {
            items: vec![],
            dependencies: vec![],
        })
    }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: TokioSelectConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;
        config.validate()?;

        let fn_name = format_ident!("select_{}", config.name);
        let vis_tokens = config.visibility.as_tokens();

        let mut param_toks = Vec::new();
        for param in &config.params {
            let p_name = format_ident!("{}", param.name);
            let p_ty: syn::Type = parse_str(&param.ty).unwrap();
            param_toks.push(quote! { #p_name: #p_ty });
        }

        let mut select_branches = Vec::new();
        for branch in &config.branches {
            let pat_expr: syn::Expr = parse_str(&branch.pattern)
                .map_err(|e| TemplateError::ConfigMismatch(format!("tokio.select: pattern invalid: {e}")))?;
            let body_block: syn::Block = parse_str(&format!("{{ {} }}", branch.body))
                .map_err(|e| TemplateError::ConfigMismatch(format!("tokio.select: branch body invalid: {e}")))?;
            select_branches.push(quote! {
                #pat_expr => #body_block
            });
        }

        let tokens = quote! {
            #vis_tokens async fn #fn_name(#(#param_toks),*) {
                tokio::select! {
                    #(#select_branches),*
                }
            }
        };

        let formatted = format_tokens("tokio.select", tokens)?;

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("runtime/{}.rs", config.name),
                source: formatted,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ===========================================================================
// tokio.join
// ===========================================================================

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct TokioJoinConfig {
    pub(super) name: String,
    #[serde(default)]
    pub(super) params: Vec<SpawnParam>,
    pub(super) futures: Vec<String>,
    #[serde(default)]
    pub(super) visibility: Visibility,
}

impl TokioJoinConfig {
    fn validate(&self) -> Result<(), TemplateError> {
        validate_helper_name("tokio.join", "name", &self.name)?;
        for param in &self.params {
            validate_param("tokio.join", param)?;
        }
        if self.futures.is_empty() {
            return Err(TemplateError::ConfigMismatch(
                "tokio.join: must configure at least one future expression".into()
            ));
        }
        for (i, fut) in self.futures.iter().enumerate() {
            if parse_str::<syn::Expr>(fut).is_err() {
                return Err(TemplateError::ConfigMismatch(format!(
                    "tokio.join: future expression {} is invalid Rust: {:?}",
                    i, fut
                )));
            }
        }
        Ok(())
    }
}

pub struct TokioJoin {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl TokioJoin {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("tokio.join").expect("tokio.join id is static and valid"),
            display: TemplateDisplay::new(
                "Tokio Join",
                "Tokio",
                "Waits on multiple concurrent futures. Emits a join helper into src/runtime/.",
            ),
            inputs: vec![],
            outputs: vec![],
            schema: serde_json::to_value(schemars::schema_for!(TokioJoinConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for TokioJoin {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::PassThrough }

    fn emit_schema(&self, _ctx: &CodegenCtx<'_>) -> Result<SchemaEmission, TemplateError> {
        Ok(SchemaEmission {
            items: vec![],
            dependencies: vec![],
        })
    }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: TokioJoinConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;
        config.validate()?;

        let fn_name = format_ident!("join_{}", config.name);
        let vis_tokens = config.visibility.as_tokens();

        let mut param_toks = Vec::new();
        for param in &config.params {
            let p_name = format_ident!("{}", param.name);
            let p_ty: syn::Type = parse_str(&param.ty).unwrap();
            param_toks.push(quote! { #p_name: #p_ty });
        }

        let mut fut_exprs = Vec::new();
        for fut in &config.futures {
            let expr: syn::Expr = parse_str(fut).unwrap();
            fut_exprs.push(quote! { #expr });
        }

        let tokens = quote! {
            #vis_tokens async fn #fn_name(#(#param_toks),*) {
                tokio::join!(#(#fut_exprs),*);
            }
        };

        let formatted = format_tokens("tokio.join", tokens)?;

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("runtime/{}.rs", config.name),
                source: formatted,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ===========================================================================
// tokio.spawn_blocking
// ===========================================================================

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct TokioSpawnBlockingConfig {
    pub(super) name: String,
    #[serde(default)]
    pub(super) params: Vec<SpawnParam>,
    pub(super) body: String,
    pub(super) return_type: String,
    #[serde(default)]
    pub(super) visibility: Visibility,
}

impl TokioSpawnBlockingConfig {
    fn validate(&self) -> Result<(), TemplateError> {
        validate_helper_name("tokio.spawn_blocking", "name", &self.name)?;
        for param in &self.params {
            validate_param("tokio.spawn_blocking", param)?;
        }
        validate_type("tokio.spawn_blocking", "return_type", &self.return_type)?;
        
        let check_expr = format!("move || -> {} {{ {} }}", self.return_type, self.body);
        if parse_str::<syn::Expr>(&check_expr).is_err() {
            return Err(TemplateError::ConfigMismatch(format!(
                "tokio.spawn_blocking: body contains invalid Rust: {:?}",
                self.body
            )));
        }
        Ok(())
    }
}

pub struct TokioSpawnBlocking {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl TokioSpawnBlocking {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("tokio.spawn_blocking").expect("tokio.spawn_blocking id is static and valid"),
            display: TemplateDisplay::new(
                "Tokio Spawn Blocking",
                "Tokio",
                "Spawns a synchronous blocking computation. Emits a spawn blocking helper into src/runtime/.",
            ),
            inputs: vec![],
            outputs: vec![],
            schema: serde_json::to_value(schemars::schema_for!(TokioSpawnBlockingConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for TokioSpawnBlocking {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::PassThrough }

    fn emit_schema(&self, _ctx: &CodegenCtx<'_>) -> Result<SchemaEmission, TemplateError> {
        Ok(SchemaEmission {
            items: vec![],
            dependencies: vec![],
        })
    }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: TokioSpawnBlockingConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;
        config.validate()?;

        let fn_name = format!("spawn_blocking_{}", config.name);
        let vis_str = config.visibility.as_tokens().to_string();

        let mut params_vec = Vec::new();
        let mut clones_vec = Vec::new();

        for param in &config.params {
            params_vec.push(format!("{}: {}", param.name, param.ty));
            if config.body.contains(&param.name) {
                clones_vec.push(format!("let {} = /*[clone:{}]*/{};", param.name, param.name, param.name));
            }
        }

        let params_str = params_vec.join(", ");
        let clones_str = if clones_vec.is_empty() {
            String::new()
        } else {
            format!("{}\n    ", clones_vec.join("\n    "))
        };

        let source = format!(
            "{} async fn {}({}) -> Result<{}, tokio::task::JoinError> {{\n    {}tokio::task::spawn_blocking(move || {{\n        {}\n    }}).await\n}}\n",
            vis_str, fn_name, params_str, config.return_type, clones_str, config.body
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("runtime/{}.rs", config.name),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ===========================================================================
// tokio.semaphore
// ===========================================================================

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct TokioSemaphoreConfig {
    pub(super) name: String,
    pub(super) permits: usize,
    #[serde(default)]
    pub(super) visibility: Visibility,
}

impl TokioSemaphoreConfig {
    fn validate(&self) -> Result<(), TemplateError> {
        validate_helper_name("tokio.semaphore", "name", &self.name)?;
        if self.permits == 0 {
            return Err(TemplateError::ConfigMismatch(
                "tokio.semaphore: permits must be >= 1".into()
            ));
        }
        Ok(())
    }
}

pub struct TokioSemaphore {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl TokioSemaphore {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("tokio.semaphore").expect("tokio.semaphore id is static and valid"),
            display: TemplateDisplay::new(
                "Tokio Semaphore",
                "Tokio",
                "Limits concurrent access via permits. Emits a constructor helper returning tokio::sync::Semaphore into src/runtime/.",
            ),
            inputs: vec![],
            outputs: vec![],
            schema: serde_json::to_value(schemars::schema_for!(TokioSemaphoreConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for TokioSemaphore {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::PassThrough }

    fn emit_schema(&self, _ctx: &CodegenCtx<'_>) -> Result<SchemaEmission, TemplateError> {
        Ok(SchemaEmission {
            items: vec![],
            dependencies: vec![],
        })
    }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: TokioSemaphoreConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;
        config.validate()?;

        let fn_name = format_ident!("make_{}", config.name);
        let vis_tokens = config.visibility.as_tokens();
        let permits = config.permits;

        let tokens = quote! {
            #vis_tokens fn #fn_name() -> tokio::sync::Semaphore {
                tokio::sync::Semaphore::new(#permits)
            }
        };

        let formatted = format_tokens("tokio.semaphore", tokens)?;

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("runtime/{}.rs", config.name),
                source: formatted,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ===========================================================================
// tokio.notify
// ===========================================================================

#[derive(Debug, Clone, JsonSchema, Deserialize)]
pub(super) struct TokioNotifyConfig {
    pub(super) name: String,
    #[serde(default)]
    pub(super) visibility: Visibility,
}

impl TokioNotifyConfig {
    fn validate(&self) -> Result<(), TemplateError> {
        validate_helper_name("tokio.notify", "name", &self.name)?;
        Ok(())
    }
}

pub struct TokioNotify {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl TokioNotify {
    pub fn new() -> Self {
        Self {
            id: TemplateId::new("tokio.notify").expect("tokio.notify id is static and valid"),
            display: TemplateDisplay::new(
                "Tokio Notify",
                "Tokio",
                "Notification signal primitive. Emits a constructor helper returning tokio::sync::Notify into src/runtime/.",
            ),
            inputs: vec![],
            outputs: vec![],
            schema: serde_json::to_value(schemars::schema_for!(TokioNotifyConfig))
                .expect("schemars output is valid JSON"),
        }
    }
}

impl NodeTemplate for TokioNotify {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Runtime }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::PassThrough }

    fn emit_schema(&self, _ctx: &CodegenCtx<'_>) -> Result<SchemaEmission, TemplateError> {
        Ok(SchemaEmission {
            items: vec![],
            dependencies: vec![],
        })
    }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: TokioNotifyConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;
        config.validate()?;

        let fn_name = format_ident!("make_{}", config.name);
        let vis_tokens = config.visibility.as_tokens();

        let tokens = quote! {
            #vis_tokens fn #fn_name() -> tokio::sync::Notify {
                tokio::sync::Notify::new()
            }
        };

        let formatted = format_tokens("tokio.notify", tokens)?;

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("runtime/{}.rs", config.name),
                source: formatted,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ===========================================================================
// tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::types::{Graph, Node, NodeId, Position, GRAPH_SCHEMA_VERSION};
    use serde_json::json;
    use std::path::PathBuf;

    /// Build a single-node `(root, graph, node)` triple for tests. Mirrors
    /// the helper in `language.rs::tests`; `CodegenCtx` needs all four
    /// fields populated even when the template only reads `node.config`.
    fn ctx(template_id: &str, config: Value) -> (PathBuf, Graph, Node) {
        let node = Node {
            id: NodeId("n1".to_string()),
            template_id: TemplateId::new(template_id).expect("valid id"),
            position: Position { x: 0.0, y: 0.0 },
            config,
            label: None,
        };
        let graph = Graph {
            schema_version: GRAPH_SCHEMA_VERSION,
            nodes: vec![node.clone()],
            edges: Vec::new(),
        };
        (PathBuf::from("/tmp/out"), graph, node)
    }

    fn emit_mpsc(config: Value) -> Result<RuntimeEmission, TemplateError> {
        let (root, graph, node) = ctx("tokio.mpsc", config);
        let template = TokioMpsc::new();
        template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        })
    }

    fn emit_mutex(config: Value) -> Result<RuntimeEmission, TemplateError> {
        let (root, graph, node) = ctx("tokio.mutex", config);
        let template = TokioMutex::new();
        template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        })
    }

    fn emit_rwlock(config: Value) -> Result<RuntimeEmission, TemplateError> {
        let (root, graph, node) = ctx("tokio.rwlock", config);
        let template = TokioRwLock::new();
        template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        })
    }

    fn emit_spawn(config: Value) -> Result<RuntimeEmission, TemplateError> {
        let (root, graph, node) = ctx("tokio.spawn", config);
        let template = TokioSpawn::new();
        template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        })
    }

    fn emit_sleep(config: Value) -> Result<RuntimeEmission, TemplateError> {
        let (root, graph, node) = ctx("tokio.sleep", config);
        let template = TokioSleep::new();
        template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        })
    }

    fn emit_interval(config: Value) -> Result<RuntimeEmission, TemplateError> {
        let (root, graph, node) = ctx("tokio.interval", config);
        let template = TokioInterval::new();
        template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        })
    }

    fn emit_select(config: Value) -> Result<RuntimeEmission, TemplateError> {
        let (root, graph, node) = ctx("tokio.select", config);
        let template = TokioSelect::new();
        template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        })
    }

    fn emit_join(config: Value) -> Result<RuntimeEmission, TemplateError> {
        let (root, graph, node) = ctx("tokio.join", config);
        let template = TokioJoin::new();
        template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        })
    }

    fn emit_spawn_blocking(config: Value) -> Result<RuntimeEmission, TemplateError> {
        let (root, graph, node) = ctx("tokio.spawn_blocking", config);
        let template = TokioSpawnBlocking::new();
        template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        })
    }

    fn emit_semaphore(config: Value) -> Result<RuntimeEmission, TemplateError> {
        let (root, graph, node) = ctx("tokio.semaphore", config);
        let template = TokioSemaphore::new();
        template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        })
    }

    fn emit_notify(config: Value) -> Result<RuntimeEmission, TemplateError> {
        let (root, graph, node) = ctx("tokio.notify", config);
        let template = TokioNotify::new();
        template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        })
    }

    #[test]
    fn mutex_emits_correct_constructor() {
        let out = emit_mutex(json!({
            "name": "state",
            "item_type": "u64",
            "initial_value": "42"
        })).unwrap();
        let src = &out.items[0].source;
        assert!(src.contains("pub fn make_state() -> tokio::sync::Mutex<u64>"));
        assert!(src.contains("tokio::sync::Mutex::new(42)"));
    }

    #[test]
    fn rwlock_emits_correct_constructor() {
        let out = emit_rwlock(json!({
            "name": "cache",
            "item_type": "String",
            "initial_value": "String::new()"
        })).unwrap();
        let src = &out.items[0].source;
        assert!(src.contains("pub fn make_cache() -> tokio::sync::RwLock<String>"));
        assert!(src.contains("tokio::sync::RwLock::new(String::new())"));
    }

    #[test]
    fn spawn_emits_correct_spawn_helper() {
        let out = emit_spawn(json!({
            "name": "worker",
            "params": [
                { "name": "my_chan", "ty": "tokio::sync::mpsc::Sender<u64>" }
            ],
            "body": "loop { let _ = my_chan.send(1).await; }"
        })).unwrap();
        let src = &out.items[0].source;
        assert!(src.contains("pub fn spawn_worker"));
        assert!(src.contains("my_chan: tokio::sync::mpsc::Sender<u64>"));
        assert!(src.contains("let my_chan = /*[clone:my_chan]*/my_chan;"));
        assert!(src.contains("tokio::spawn(async move {"));
    }

    #[test]
    fn sleep_emits_correct_fn() {
        let out = emit_sleep(json!({
            "name": "delay",
            "duration_ms": 1000
        })).unwrap();
        let src = &out.items[0].source;
        assert!(src.contains("pub async fn sleep_delay()"));
        assert!(src.contains("tokio::time::sleep(tokio::time::Duration::from_millis(1000u64)).await;"));
    }

    #[test]
    fn interval_emits_correct_constructor() {
        let out = emit_interval(json!({
            "name": "ticker",
            "period_ms": 500
        })).unwrap();
        let src = &out.items[0].source;
        assert!(src.contains("pub fn make_ticker() -> tokio::time::Interval"));
        assert!(src.contains("tokio::time::interval(tokio::time::Duration::from_millis(500u64))"));
    }

    #[test]
    fn select_emits_correct_fn() {
        let out = emit_select(json!({
            "name": "poll_events",
            "params": [
                { "name": "rx1", "ty": "tokio::sync::mpsc::Receiver<u64>" },
                { "name": "rx2", "ty": "tokio::sync::mpsc::Receiver<String>" }
            ],
            "branches": [
                { "pattern": "Some(v) = rx1.recv()", "body": "println!(\"rx1: {}\", v);" },
                { "pattern": "Some(s) = rx2.recv()", "body": "println!(\"rx2: {}\", s);" }
            ]
        })).unwrap();
        let src = &out.items[0].source;
        assert!(src.contains("pub async fn select_poll_events("));
        assert!(src.contains("tokio::select!"));
        assert!(src.contains("Some(v) = rx1.recv() => {"));
        assert!(src.contains("Some(s) = rx2.recv() => {"));
    }

    #[test]
    fn join_emits_correct_fn() {
        let out = emit_join(json!({
            "name": "both",
            "futures": [
                "async { 1 }",
                "async { 2 }"
            ]
        })).unwrap();
        let src = &out.items[0].source;
        assert!(src.contains("pub async fn join_both()"));
        assert!(src.contains("tokio::join!(async { 1 }, async { 2 });"));
    }

    #[test]
    fn spawn_blocking_emits_correct_fn() {
        let out = emit_spawn_blocking(json!({
            "name": "load_file",
            "params": [
                { "name": "path", "ty": "String" }
            ],
            "body": "std::fs::read_to_string(path).unwrap()",
            "return_type": "String"
        })).unwrap();
        let src = &out.items[0].source;
        assert!(src.contains("pub async fn spawn_blocking_load_file("));
        assert!(src.contains("let path = /*[clone:path]*/path;"));
        assert!(src.contains("-> Result<String, tokio::task::JoinError>"));
        assert!(src.contains("tokio::task::spawn_blocking(move || {"));
    }

    #[test]
    fn semaphore_emits_correct_constructor() {
        let out = emit_semaphore(json!({
            "name": "limit",
            "permits": 10
        })).unwrap();
        let src = &out.items[0].source;
        assert!(src.contains("pub fn make_limit() -> tokio::sync::Semaphore"));
        assert!(src.contains("tokio::sync::Semaphore::new(10usize)"));
    }

    #[test]
    fn notify_emits_correct_constructor() {
        let out = emit_notify(json!({
            "name": "event"
        })).unwrap();
        let src = &out.items[0].source;
        assert!(src.contains("pub fn make_event() -> tokio::sync::Notify"));
        assert!(src.contains("tokio::sync::Notify::new()"));
    }

    #[test]
    fn mpsc_emits_typed_channel_constructor() {
        let out = emit_mpsc(json!({
            "name": "orders",
            "item_type": "Order",
            "buffer": 128
        }))
        .expect("valid config emits");
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.items[0].module_path, "runtime/orders.rs");
        let src = &out.items[0].source;
        assert!(src.contains("pub fn make_orders()"), "src: {src}");
        assert!(src.contains("tokio::sync::mpsc::Sender<Order>"), "src: {src}");
        assert!(src.contains("tokio::sync::mpsc::Receiver<Order>"), "src: {src}");
        assert!(src.contains("tokio::sync::mpsc::channel(128"), "src: {src}");
    }

    #[test]
    fn mpsc_emission_is_byte_stable() {
        let a = emit_mpsc(json!({
            "name": "events", "item_type": "u64", "buffer": 32
        })).unwrap();
        let b = emit_mpsc(json!({
            "name": "events", "item_type": "u64", "buffer": 32
        })).unwrap();
        assert_eq!(a.items[0].source, b.items[0].source);
    }

    #[test]
    fn mpsc_emits_no_extra_dependencies() {
        // tokio is in baseline; nothing to add.
        let out = emit_mpsc(json!({
            "name": "x", "item_type": "u8", "buffer": 1
        })).unwrap();
        assert!(out.dependencies.is_empty(), "deps: {:?}", out.dependencies);
    }

    #[test]
    fn mpsc_accepts_generic_item_type() {
        let out = emit_mpsc(json!({
            "name": "results",
            "item_type": "Result<u64, std::io::Error>",
            "buffer": 16
        })).expect("generic type accepted");
        assert!(out.items[0].source.contains("Result<u64, std::io::Error>"));
    }

    #[test]
    fn mpsc_accepts_nested_generic_item_type() {
        let out = emit_mpsc(json!({
            "name": "nested",
            "item_type": "Vec<Option<crate::types::Order>>",
            "buffer": 4
        })).expect("nested generic accepted");
        assert!(out.items[0].source.contains("Vec<Option<crate::types::Order>>"));
    }

    #[test]
    fn mpsc_rejects_zero_buffer() {
        // mpsc::channel(0) panics at runtime — reject at schema time.
        let err = emit_mpsc(json!({
            "name": "bad", "item_type": "u8", "buffer": 0
        })).expect_err("buffer=0 must fail");
        match err {
            TemplateError::ConfigMismatch(msg) => {
                assert!(msg.contains("buffer must be >= 1"), "msg: {msg}")
            }
            other => panic!("expected ConfigMismatch, got {other:?}"),
        }
    }

    #[test]
    fn mpsc_rejects_camel_case_name() {
        let err = emit_mpsc(json!({
            "name": "Orders", "item_type": "u8", "buffer": 1
        })).expect_err("CamelCase name must fail");
        match err {
            TemplateError::ConfigMismatch(msg) => assert!(msg.contains("snake_case"), "msg: {msg}"),
            other => panic!("expected ConfigMismatch, got {other:?}"),
        }
    }

    #[test]
    fn mpsc_rejects_reserved_word_name() {
        // `type` passes the snake_case regex but would panic `format_ident!`.
        // Load-bearing guard inherited from S15a.
        let err = emit_mpsc(json!({
            "name": "type", "item_type": "u8", "buffer": 1
        })).expect_err("reserved word must fail");
        match err {
            TemplateError::ConfigMismatch(msg) => assert!(
                msg.contains("reserved word") || msg.contains("not a valid identifier"),
                "msg: {msg}"
            ),
            other => panic!("expected ConfigMismatch, got {other:?}"),
        }
    }

    #[test]
    fn mpsc_rejects_empty_name() {
        emit_mpsc(json!({
            "name": "", "item_type": "u8", "buffer": 1
        })).expect_err("empty name must fail");
    }

    #[test]
    fn mpsc_rejects_unparseable_item_type() {
        let err = emit_mpsc(json!({
            "name": "broken", "item_type": "Vec<{", "buffer": 1
        })).expect_err("garbage type must fail");
        match err {
            TemplateError::ConfigMismatch(msg) => {
                assert!(msg.contains("not a valid Rust type"), "msg: {msg}")
            }
            other => panic!("expected ConfigMismatch, got {other:?}"),
        }
    }

    #[test]
    fn mpsc_emission_parses_as_rust() {
        // Round-trip: the formatted source must itself parse as a syn::File.
        let out = emit_mpsc(json!({
            "name": "rt_check", "item_type": "(u64, String)", "buffer": 8
        })).unwrap();
        let parsed: Result<syn::File, _> = syn::parse_str(&out.items[0].source);
        assert!(parsed.is_ok(), "emission did not parse: {}", out.items[0].source);
    }

    #[test]
    fn mpsc_visibility_pub_crate() {
        let out = emit_mpsc(json!({
            "name": "priv_chan", "item_type": "u8", "buffer": 1,
            "visibility": "pub_crate"
        })).unwrap();
        assert!(
            out.items[0].source.contains("pub(crate) fn make_priv_chan"),
            "src: {}", out.items[0].source
        );
    }

    // ---- tokio.broadcast tests --------------------------------------------

    fn emit_broadcast(config: Value) -> Result<RuntimeEmission, TemplateError> {
        let (root, graph, node) = ctx("tokio.broadcast", config);
        let template = TokioBroadcast::new();
        template.emit_runtime(&CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &root,
            graph: &graph,
        })
    }

    #[test]
    fn broadcast_emits_typed_channel_constructor() {
        let out = emit_broadcast(json!({
            "name": "events", "item_type": "Event", "capacity": 64
        })).expect("valid config emits");
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.items[0].module_path, "runtime/events.rs");
        let src = &out.items[0].source;
        assert!(src.contains("pub fn make_events()"), "src: {src}");
        assert!(src.contains("tokio::sync::broadcast::Sender<Event>"), "src: {src}");
        assert!(src.contains("tokio::sync::broadcast::Receiver<Event>"), "src: {src}");
        assert!(src.contains("tokio::sync::broadcast::channel(64"), "src: {src}");
    }

    #[test]
    fn broadcast_is_broadcast_not_mpsc() {
        // Sanity: emit_broadcast must never accidentally call mpsc.
        let out = emit_broadcast(json!({
            "name": "x", "item_type": "u8", "capacity": 1
        })).unwrap();
        let src = &out.items[0].source;
        assert!(src.contains("broadcast"), "missing broadcast: {src}");
        assert!(!src.contains("mpsc"), "leaked mpsc into broadcast: {src}");
    }

    #[test]
    fn broadcast_emission_is_byte_stable() {
        let a = emit_broadcast(json!({
            "name": "ticks", "item_type": "u64", "capacity": 8
        })).unwrap();
        let b = emit_broadcast(json!({
            "name": "ticks", "item_type": "u64", "capacity": 8
        })).unwrap();
        assert_eq!(a.items[0].source, b.items[0].source);
    }

    #[test]
    fn broadcast_emits_no_extra_dependencies() {
        let out = emit_broadcast(json!({
            "name": "x", "item_type": "u8", "capacity": 1
        })).unwrap();
        assert!(out.dependencies.is_empty(), "deps: {:?}", out.dependencies);
    }

    #[test]
    fn broadcast_accepts_nested_generic_item_type() {
        let out = emit_broadcast(json!({
            "name": "results",
            "item_type": "Result<crate::types::Event, std::io::Error>",
            "capacity": 16
        })).expect("nested generic accepted");
        let src = &out.items[0].source;
        assert!(
            src.contains("Result<crate::types::Event, std::io::Error>"),
            "src: {src}"
        );
    }

    #[test]
    fn broadcast_rejects_zero_capacity() {
        let err = emit_broadcast(json!({
            "name": "bad", "item_type": "u8", "capacity": 0
        })).expect_err("capacity=0 must fail");
        match err {
            TemplateError::ConfigMismatch(msg) => {
                assert!(msg.contains("capacity must be >= 1"), "msg: {msg}")
            }
            other => panic!("expected ConfigMismatch, got {other:?}"),
        }
    }

    #[test]
    fn broadcast_rejects_camel_case_name() {
        emit_broadcast(json!({
            "name": "Events", "item_type": "u8", "capacity": 1
        })).expect_err("CamelCase name must fail");
    }

    #[test]
    fn broadcast_rejects_reserved_word_name() {
        emit_broadcast(json!({
            "name": "match", "item_type": "u8", "capacity": 1
        })).expect_err("reserved word must fail");
    }

    #[test]
    fn broadcast_rejects_unparseable_item_type() {
        emit_broadcast(json!({
            "name": "broken", "item_type": "Vec<{", "capacity": 1
        })).expect_err("garbage type must fail");
    }

    #[test]
    fn broadcast_emission_parses_as_rust() {
        let out = emit_broadcast(json!({
            "name": "rt_check", "item_type": "(u64, String)", "capacity": 8
        })).unwrap();
        let parsed: Result<syn::File, _> = syn::parse_str(&out.items[0].source);
        assert!(parsed.is_ok(), "emission did not parse: {}", out.items[0].source);
    }

    #[test]
    fn broadcast_visibility_pub_crate() {
        let out = emit_broadcast(json!({
            "name": "internal", "item_type": "u8", "capacity": 1,
            "visibility": "pub_crate"
        })).unwrap();
        assert!(
            out.items[0].source.contains("pub(crate) fn make_internal"),
            "src: {}", out.items[0].source
        );
    }
}
