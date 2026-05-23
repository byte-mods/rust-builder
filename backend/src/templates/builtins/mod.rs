//! Built-in node templates registered into [`crate::templates::TemplateRegistry`]
//! at studio startup.
//!
//! Seven templates ship in v1, mapping to the closed `NodeKind` enum that
//! Section 2 used. The legacy enum's serialisations are accepted by the
//! `Node` deserialiser (see S3-T5) so existing graphs continue to load.
//!
//! ## Authoring conventions
//!
//! Each template is a unit struct with three cached fields:
//! - `id: TemplateId` — once-validated, cheap-clone identifier.
//! - `display`, `input_ports`, `output_ports` — frozen at construction.
//! - `config_schema: Value` — `schemars::schema_for!` output cached as JSON.
//!
//! The trait's default `emit_runtime` / `emit_schema` placeholders are
//! inherited; real codegen bodies arrive in S4 / S5 / S7 / S9.
//!
//! ## Why a single file
//!
//! Each template is ~30 lines; spreading them across ten files inflates
//! the directory listing without aiding navigation. Splitting only makes
//! sense once an individual template grows non-trivial codegen (S4+).

use schemars::{schema_for, JsonSchema};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

use crate::templates::{
    ports::{PortMultiplicity, PortSpec},
    CodegenMode, DebugBridgeKind, NodeTemplate, TemplateDisplay, TemplateId, TemplateRegistry,
};

pub mod custom;
pub mod egress;
pub mod ingest;
pub mod language;
pub mod tokio;
pub mod connectors;
pub mod stream;
pub mod wasm;

/// Register every built-in template into `registry`. Called once at startup
/// from `TemplateRegistry::with_builtins`. Panics on duplicate id — that's
/// a builtin-author bug, must surface immediately at startup.
pub fn register_all(registry: &mut TemplateRegistry) {
    macro_rules! reg {
        ($t:expr) => {{
            let template: Arc<dyn NodeTemplate> = Arc::new($t);
            let prev = registry.insert(template);
            assert!(prev.is_none(), "duplicate builtin template id");
        }};
    }
    reg!(EntryPoint::new());
    reg!(HttpRoute::new());
    reg!(HttpHandler::new());
    reg!(CoreService::new());
    reg!(CoreDto::new());
    reg!(ObservabilityLogger::new());
    reg!(ParserJson::new());
    reg!(ParserXml::new());
    reg!(ParserProtobuf::new());
    reg!(IntegrationConsumerPlaceholder::new());
    reg!(IntegrationSchedulerPlaceholder::new());

    // S10 — real Ingest & Egress adapters
    reg!(ingest::IntegrationScheduler::new());
    reg!(ingest::IntegrationFileTail::new());
    reg!(egress::IntegrationHttpClient::new());
    reg!(egress::IntegrationDbWriter::new());

    // S15a — visual Rust language nodes.
    reg!(language::LanguageStruct::new());
    reg!(language::LanguageEnum::new());
    reg!(language::LanguageFn::new());
    reg!(language::LanguageClone::new());
    reg!(language::LanguageIfElse::new());
    reg!(language::LanguageMatch::new());
    reg!(language::LanguageLoop::new());
    reg!(language::LanguagePropagate::new());
    reg!(language::LanguageAwait::new());
    reg!(language::LanguagePointer::new());

    // S15b — Tokio runtime primitives as constructor helpers.
    reg!(tokio::TokioBroadcast::new());
    reg!(tokio::TokioMpsc::new());
    reg!(tokio::TokioMutex::new());
    reg!(tokio::TokioRwLock::new());
    reg!(tokio::TokioSpawn::new());
    reg!(tokio::TokioSleep::new());
    reg!(tokio::TokioInterval::new());
    reg!(tokio::TokioSelect::new());
    reg!(tokio::TokioJoin::new());
    reg!(tokio::TokioSpawnBlocking::new());
    reg!(tokio::TokioSemaphore::new());
    reg!(tokio::TokioNotify::new());

    // S16 — Custom Block visual template
    reg!(custom::CustomBlock::new());

    // S16/S17 — Universal Connectors
    reg!(connectors::IntegrationKafkaConsumer::new());
    reg!(connectors::IntegrationKafkaProducer::new());
    reg!(connectors::IntegrationRedis::new());
    reg!(connectors::IntegrationSqlConnector::new());

    // S23-S26 — Streaming Operators (CEP Engine)
    reg!(stream::StreamFilter::new());
    reg!(stream::StreamMap::new());
    reg!(stream::StreamSelect::new());
    reg!(stream::StreamUnion::new());
    reg!(stream::StreamJoin::new());
    reg!(stream::StreamWindow::new());
    reg!(stream::StreamPattern::new());

    // S27 — WebAssembly runner
    reg!(wasm::WasmRunner::new());
}

// ---- shared construction helper -------------------------------------------

/// Compute a template's config schema once at construction and serialise it
/// to `serde_json::Value` so the trait method can return `&Value` without
/// per-call work.
fn schema_value<T: JsonSchema>() -> Value {
    serde_json::to_value(schema_for!(T)).expect("schemars output is valid JSON")
}

fn id_or_panic(s: &str) -> TemplateId {
    TemplateId::new(s).expect("builtin template id must validate")
}

/// Convert CamelCase to snake_case for file paths.
fn to_snake_case(s: &str) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

// ---- core.entry_point -----------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
struct EntryPointConfig {
    /// Bind address for the HTTP server. Defaults to 127.0.0.1:8080.
    #[serde(default = "default_bind")]
    bind_address: String,
    /// Log level filter. Defaults to info.
    #[serde(default = "default_log_level")]
    log_level: String,
}

fn default_bind() -> String { "127.0.0.1:8080".into() }
fn default_log_level() -> String { "info".into() }

pub struct EntryPoint {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl EntryPoint {
    fn new() -> Self {
        Self {
            id: id_or_panic("core.entry_point"),
            display: TemplateDisplay::new(
                "Entry Point",
                "Core",
                "Application entry point — represents main.rs. Wire routes, consumers, schedulers, and services here to show how your application starts up.",
            ),
            inputs: vec![],
            outputs: vec![
                PortSpec::single("http", "any", "HTTP server routes — wire http.route nodes here."),
                PortSpec::single("consumer", "any", "Background consumers — wire Kafka consumers here."),
                PortSpec::single("scheduler", "any", "Scheduled jobs — wire cron schedulers here."),
                PortSpec::single("service", "any", "Background services — wire core.service nodes here."),
            ],
            schema: schema_value::<EntryPointConfig>(),
        }
    }
}

impl NodeTemplate for EntryPoint {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }

    fn emit_runtime(
        &self,
        _ctx: &crate::templates::codegen::CodegenCtx<'_>,
    ) -> Result<crate::templates::codegen::RuntimeEmission, crate::templates::TemplateError> {
        // The entry point is purely visual — main.rs is generated by
        // bootstrap::main_rs() based on the graph structure, not this node.
        Ok(crate::templates::codegen::RuntimeEmission {
            items: vec![],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---- http.route -----------------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)] // fields read by codegen in S4
struct HttpRouteConfig {
    /// HTTP path, e.g. `/hello/:name`. Validated by the generator in S4.
    path: String,
    /// HTTP method. Only the common verbs are listed; extend in S4 if
    /// PATCH/OPTIONS/HEAD are needed.
    method: HttpMethod,
}

#[derive(Debug, JsonSchema, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
#[allow(dead_code)]
enum HttpMethod { Get, Post, Put, Delete }

pub struct HttpRoute {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl HttpRoute {
    fn new() -> Self {
        Self {
            id: id_or_panic("http.route"),
            display: TemplateDisplay::new(
                "HTTP Route",
                "HTTP",
                "Mounts an HTTP endpoint at a given path and method, forwarding to a handler.",
            ),
            inputs: vec![PortSpec::single(
                "entry",
                "any",
                "Application entry — wire from core.entry_point to show this route is part of the server.",
            )],
            outputs: vec![PortSpec::single(
                "request",
                "http.request",
                "Incoming HTTP request — connect to a handler node.",
            )],
            schema: schema_value::<HttpRouteConfig>(),
        }
    }
}

impl NodeTemplate for HttpRoute {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::PassThrough }

    fn emit_runtime(
        &self,
        _ctx: &crate::templates::codegen::CodegenCtx<'_>,
    ) -> Result<crate::templates::codegen::RuntimeEmission, crate::templates::TemplateError> {
        // The orchestrator wires routes into lib.rs by reading the graph.
        Ok(crate::templates::codegen::RuntimeEmission {
            items: vec![],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---- http.handler ---------------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
struct HttpHandlerConfig {
    /// Snake_case Rust function name for the generated handler, e.g. `hello`.
    name: String,
}

pub struct HttpHandler {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl HttpHandler {
    fn new() -> Self {
        Self {
            id: id_or_panic("http.handler"),
            display: TemplateDisplay::new(
                "HTTP Handler",
                "HTTP",
                "Receives an HTTP request, calls services, returns an HTTP response.",
            ),
            inputs: vec![PortSpec::single("request", "http.request", "HTTP request payload.")],
            outputs: vec![PortSpec::single("response", "http.response", "HTTP response payload.")],
            schema: schema_value::<HttpHandlerConfig>(),
        }
    }
}

impl NodeTemplate for HttpHandler {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }

    fn emit_runtime(
        &self,
        ctx: &crate::templates::codegen::CodegenCtx<'_>,
    ) -> Result<crate::templates::codegen::RuntimeEmission, crate::templates::TemplateError> {
        let config: HttpHandlerConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| crate::templates::TemplateError::ConfigMismatch(e.to_string()))?;

        let mut uses = String::new();
        let mut body = String::new();

        // S7: handlers return Result<impl IntoResponse, AppError> so Axum can
        // render typed errors. When wired to a service, the call uses `?` to
        // propagate service failures up to the HTTP boundary.
        for upstream in ctx.graph.upstream_of(&ctx.node.id, "request") {
            if upstream.template_id.as_str() == "core.service" {
                let svc: CoreServiceConfig = serde_json::from_value(upstream.config.clone())
                    .map_err(|e| crate::templates::TemplateError::ConfigMismatch(e.to_string()))?;
                uses.push_str(&format!("use crate::services::{};\n", svc.name));
                body.push_str(&format!("    let _ = {}::{}().await?;\n", svc.name, svc.name));
            }
        }

        let source = if uses.is_empty() {
            format!(
                "use axum::response::IntoResponse;\nuse crate::errors::AppError;\n\npub async fn {}() -> Result<impl IntoResponse, AppError> {{\n    Ok(\"ok\")\n}}\n",
                config.name
            )
        } else {
            format!(
                "use axum::response::IntoResponse;\nuse crate::errors::AppError;\n{}\npub async fn {}() -> Result<impl IntoResponse, AppError> {{\n{}    Ok(\"ok\")\n}}\n",
                uses, config.name, body
            )
        };

        Ok(crate::templates::codegen::RuntimeEmission {
            items: vec![crate::templates::codegen::EmittedItem {
                module_path: format!("handlers/{}.rs", config.name),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---- core.service ---------------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
struct CoreServiceConfig {
    /// Snake_case Rust module name for the service.
    name: String,
    #[serde(default)]
    description: String,
}

pub struct CoreService {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl CoreService {
    fn new() -> Self {
        Self {
            id: id_or_panic("core.service"),
            display: TemplateDisplay::new(
                "Service",
                "Core",
                "Business-logic unit. Lives in src/services/; called by handlers and other services.",
            ),
            inputs: vec![PortSpec {
                name: "input".into(),
                type_tag: "any".into(),
                multiplicity: PortMultiplicity::Many,
                doc: "Inputs from upstream nodes; service body wires them together.".into(),
            }],
            outputs: vec![PortSpec::single("output", "any", "Result emitted by this service.")],
            schema: schema_value::<CoreServiceConfig>(),
        }
    }
}

impl NodeTemplate for CoreService {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }

    fn emit_runtime(
        &self,
        ctx: &crate::templates::codegen::CodegenCtx<'_>,
    ) -> Result<crate::templates::codegen::RuntimeEmission, crate::templates::TemplateError> {
        let config: CoreServiceConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| crate::templates::TemplateError::ConfigMismatch(e.to_string()))?;

        let mut uses = String::new();
        let mut body = String::new();

        // S7: services return Result<T, AppError> so handlers (and other
        // callers) can propagate failures with `?` instead of silently
        // discarding them.
        for upstream in ctx.graph.upstream_of(&ctx.node.id, "input") {
            if upstream.template_id.as_str() == "core.dto" {
                let dto: CoreDtoConfig = serde_json::from_value(upstream.config.clone())
                    .map_err(|e| crate::templates::TemplateError::ConfigMismatch(e.to_string()))?;
                let snake = to_snake_case(&dto.name);
                uses.push_str(&format!("use crate::dto::{}::{};\n", snake, dto.name));
                body.push_str(&format!("    let _: Option<{}> = None;\n", dto.name));
            }
        }

        let source = if uses.is_empty() {
            format!(
                "use crate::errors::AppError;\n\npub async fn {}() -> Result<&'static str, AppError> {{\n    Ok(\"ok\")\n}}\n",
                config.name
            )
        } else {
            format!(
                "use crate::errors::AppError;\n{}pub async fn {}() -> Result<&'static str, AppError> {{\n{}    Ok(\"ok\")\n}}\n",
                uses, config.name, body
            )
        };

        Ok(crate::templates::codegen::RuntimeEmission {
            items: vec![crate::templates::codegen::EmittedItem {
                module_path: format!("services/{}.rs", config.name),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---- core.dto -------------------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
struct CoreDtoConfig {
    /// CamelCase Rust struct name for the generated DTO.
    name: String,
    /// Ordered field list — each becomes a struct field in src/dto/.
    fields: Vec<CoreDtoField>,
}

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
struct CoreDtoField {
    name: String,
    /// Rust type literal — the generator validates this in S4. v1 accepts
    /// any string here; S4 will tighten it to a parseable Rust type.
    ty: String,
}

pub struct CoreDto {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl CoreDto {
    fn new() -> Self {
        Self {
            id: id_or_panic("core.dto"),
            display: TemplateDisplay::new(
                "DTO",
                "Core",
                "Data-transfer struct. Defines a named record reused by handlers, services, and parsers.",
            ),
            // DTOs have no runtime ports — they're definitions referenced
            // by other nodes via the `name` field. Edges pointing AT a DTO
            // node are read by codegen, not at runtime.
            inputs: vec![],
            outputs: vec![],
            schema: schema_value::<CoreDtoConfig>(),
        }
    }
}

impl NodeTemplate for CoreDto {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Codegen }

    fn emit_runtime(
        &self,
        _ctx: &crate::templates::codegen::CodegenCtx<'_>,
    ) -> Result<crate::templates::codegen::RuntimeEmission, crate::templates::TemplateError> {
        // CoreDto emits via emit_schema (Codegen mode). Runtime emission is empty.
        Ok(crate::templates::codegen::RuntimeEmission {
            items: vec![],
            dependencies: vec![],
            debug_site: None,
        })
    }

    fn emit_schema(
        &self,
        ctx: &crate::templates::codegen::CodegenCtx<'_>,
    ) -> Result<crate::templates::codegen::SchemaEmission, crate::templates::TemplateError> {
        let config: CoreDtoConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| crate::templates::TemplateError::ConfigMismatch(e.to_string()))?;

        let snake_name = to_snake_case(&config.name);
        let mut fields = String::new();
        for f in &config.fields {
            fields.push_str(&format!("    pub {}: {},\n", f.name, f.ty));
        }

        let source = format!(
            "use serde::{{Deserialize, Serialize}};\n\n#[derive(Debug, Clone, Serialize, Deserialize)]\npub struct {} {{\n{}}}\n",
            config.name, fields
        );

        Ok(crate::templates::codegen::SchemaEmission {
            items: vec![crate::templates::codegen::EmittedItem {
                module_path: format!("dto/{}.rs", snake_name),
                source,
            }],
            dependencies: vec![],
        })
    }
}

// ---- observability.logger -------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
struct LoggerConfig {
    level: LogLevel,
    format: LogFormat,
    /// Snake_case module name for the generated logger. Defaults to "logger".
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, JsonSchema, Deserialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum LogLevel { Trace, Debug, Info, Warn, Error }

#[derive(Debug, JsonSchema, Deserialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum LogFormat { Pretty, Json }

pub struct ObservabilityLogger {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl ObservabilityLogger {
    fn new() -> Self {
        Self {
            id: id_or_panic("observability.logger"),
            display: TemplateDisplay::new(
                "Logger",
                "Observability",
                "Records the value flowing through it at the configured level. Pass-through — does not modify the data.",
            ),
            inputs: vec![PortSpec::single("value", "any", "Value to log; emitted unchanged on the output port.")],
            outputs: vec![PortSpec::single("value", "any", "The same value, unchanged.")],
            schema: schema_value::<LoggerConfig>(),
        }
    }
}

impl NodeTemplate for ObservabilityLogger {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::PassThrough }

    fn emit_runtime(
        &self,
        ctx: &crate::templates::codegen::CodegenCtx<'_>,
    ) -> Result<crate::templates::codegen::RuntimeEmission, crate::templates::TemplateError> {
        let config: LoggerConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| crate::templates::TemplateError::ConfigMismatch(e.to_string()))?;

        let name = config.name.unwrap_or_else(|| "logger".to_string());
        let level = match config.level {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        };

        let source = format!(
            "/// Pass-through logger — records the value at the `{level}` level and returns it unchanged.\npub fn record<T: std::fmt::Debug>(value: T) -> T {{\n    tracing::{level}!(?value, \"logged\");\n    value\n}}\n",
        );

        Ok(crate::templates::codegen::RuntimeEmission {
            items: vec![crate::templates::codegen::EmittedItem {
                module_path: format!("loggers/{}.rs", to_snake_case(&name)),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---- parser.json ----------------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
struct ParserJsonConfig {
    /// Path to the JSON schema/example file relative to project root.
    schema_file: String,
    /// Snake_case module name. Defaults to "json_parser".
    #[serde(default)]
    name: Option<String>,
}

pub struct ParserJson {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl ParserJson {
    fn new() -> Self {
        Self {
            id: id_or_panic("parser.json"),
            display: TemplateDisplay::new(
                "JSON Parser",
                "Parser",
                "Reads a JSON file at compile time via include_str! and provides a serde_json::Value parser function.",
            ),
            inputs: vec![],
            outputs: vec![PortSpec::single("value", "json", "Parsed JSON value.")],
            schema: schema_value::<ParserJsonConfig>(),
        }
    }
}

impl NodeTemplate for ParserJson {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Codegen }

    fn emit_schema(
        &self,
        ctx: &crate::templates::codegen::CodegenCtx<'_>,
    ) -> Result<crate::templates::codegen::SchemaEmission, crate::templates::TemplateError> {
        // S9: JSON Schema → Rust structs via typify.
        // The schema file is read from disk, parsed as JSON, converted to
        // schemars::schema::RootSchema, and fed into typify::TypeSpace.
        // Generated code is gated through syn + prettyplease before writing.
        let config: ParserJsonConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| crate::templates::TemplateError::ConfigMismatch(e.to_string()))?;
        let name = config.name.unwrap_or_else(|| "json_parser".to_string());

        // TODO(S9-followup): This is blocking I/O inside an async call chain.
        // For v1 we accept the trade-off because schema files are small and
        // parser nodes are rare. Future refactor: pre-read schema files in
        // Generator::generate_project via tokio::fs and pass contents through
        // CodegenCtx.
        let schema_path = ctx.output_root.join(&config.schema_file);
        let schema_text = std::fs::read_to_string(&schema_path)
            .map_err(|e| crate::templates::TemplateError::ConfigMismatch(
                format!("cannot read JSON schema file '{}': {e}", config.schema_file)
            ))?;
        let schema: schemars::schema::RootSchema = serde_json::from_str(&schema_text)
            .map_err(|e| crate::templates::TemplateError::ConfigMismatch(
                format!("JSON schema file '{}' is not valid JSON: {e}", config.schema_file)
            ))?;

        let settings = typify::TypeSpaceSettings::default();
        let mut type_space = typify::TypeSpace::new(&settings);
        type_space.add_root_schema(schema)
            .map_err(|e| crate::templates::TemplateError::ConfigMismatch(
                format!("JSON schema file '{}' could not be converted to Rust types: {e}", config.schema_file)
            ))?;

        let tokens = type_space.to_stream();
        let raw_source = tokens.to_string();

        // Gate through syn + prettyplease for formatting and validation.
        let source = crate::codegen::format::validate_and_format(
            &raw_source,
            self.id().as_str(),
            &ctx.node.id.0,
        )
        .map_err(|e| crate::templates::TemplateError::ConfigMismatch(
            format!("typify emitted malformed Rust for '{}': {e}", config.schema_file)
        ))?;

        let mut deps: Vec<(String, String)> = Vec::new();
        if type_space.uses_chrono() {
            deps.push(("chrono".to_string(), "0.4".to_string()));
        }
        if type_space.uses_regress() {
            deps.push(("regress".to_string(), "0.11".to_string()));
        }
        if type_space.uses_uuid() {
            deps.push(("uuid".to_string(), "1".to_string()));
        }
        // serde and serde_json are already in baseline deps; no need to add them.

        Ok(crate::templates::codegen::SchemaEmission {
            items: vec![crate::templates::codegen::EmittedItem {
                module_path: format!("parsers/{}.rs", to_snake_case(&name)),
                source,
            }],
            dependencies: deps,
        })
    }
}

// ---- parser.xml -----------------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
struct ParserXmlConfig {
    /// Path to the XML file relative to project root.
    schema_file: String,
    /// Snake_case module name. Defaults to "xml_parser".
    #[serde(default)]
    name: Option<String>,
}

pub struct ParserXml {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl ParserXml {
    fn new() -> Self {
        Self {
            id: id_or_panic("parser.xml"),
            display: TemplateDisplay::new(
                "XML Parser",
                "Parser",
                "Reads an XML file at compile time via include_str! and exposes the raw string.",
            ),
            inputs: vec![],
            outputs: vec![PortSpec::single("value", "xml", "Raw XML string.")],
            schema: schema_value::<ParserXmlConfig>(),
        }
    }
}

impl NodeTemplate for ParserXml {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Codegen }

    fn emit_schema(
        &self,
        ctx: &crate::templates::codegen::CodegenCtx<'_>,
    ) -> Result<crate::templates::codegen::SchemaEmission, crate::templates::TemplateError> {
        let config: ParserXmlConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| crate::templates::TemplateError::ConfigMismatch(e.to_string()))?;
        let name = config.name.unwrap_or_else(|| "xml_parser".to_string());
        let source = format!(
            "/// Raw XML from `{}`.\npub static RAW: &str = include_str!(\"../../{}\");\n",
            config.schema_file, config.schema_file
        );
        Ok(crate::templates::codegen::SchemaEmission {
            items: vec![crate::templates::codegen::EmittedItem {
                module_path: format!("parsers/{}.rs", to_snake_case(&name)),
                source,
            }],
            dependencies: vec![],
        })
    }
}

// ---- parser.protobuf ------------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
struct ParserProtobufConfig {
    /// Path to the .proto file relative to project root.
    schema_file: String,
    /// Snake_case module name. Defaults to "protobuf_parser".
    #[serde(default)]
    name: Option<String>,
}

pub struct ParserProtobuf {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl ParserProtobuf {
    fn new() -> Self {
        Self {
            id: id_or_panic("parser.protobuf"),
            display: TemplateDisplay::new(
                "Protobuf Parser",
                "Parser",
                "Embeds a .proto file at compile time. Real prost-build codegen lands in a later section.",
            ),
            inputs: vec![],
            outputs: vec![PortSpec::single("value", "protobuf", "Raw .proto schema string.")],
            schema: schema_value::<ParserProtobufConfig>(),
        }
    }
}

impl NodeTemplate for ParserProtobuf {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn codegen_mode(&self) -> CodegenMode { CodegenMode::Codegen }

    fn emit_schema(
        &self,
        ctx: &crate::templates::codegen::CodegenCtx<'_>,
    ) -> Result<crate::templates::codegen::SchemaEmission, crate::templates::TemplateError> {
        // S9: Protobuf → Rust structs via prost-build run programmatically.
        // We compile the .proto to a temp directory, read the generated .rs
        // file(s), and embed them into the project's src/parsers/ tree.
        // This avoids forcing a build.rs workflow on users.
        let config: ParserProtobufConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| crate::templates::TemplateError::ConfigMismatch(e.to_string()))?;
        let name = config.name.unwrap_or_else(|| "protobuf_parser".to_string());

        // TODO(S9-followup): This is blocking I/O inside an async call chain.
        // For v1 we accept the trade-off because .proto files are small and
        // parser nodes are rare. Future refactor: pre-read / compile .proto
        // files in Generator::generate_project via tokio::fs / spawn_blocking.
        let proto_path = ctx.output_root.join(&config.schema_file);
        if !proto_path.exists() {
            return Err(crate::templates::TemplateError::ConfigMismatch(
                format!("protobuf schema file '{}' not found", config.schema_file)
            ));
        }

        let temp_dir = std::env::temp_dir().join(format!("prost-{}-{}", ctx.project_slug, ctx.node.id.0));
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir)
            .map_err(|e| crate::templates::TemplateError::ConfigMismatch(
                format!("failed to create temp dir for prost-build: {e}")
            ))?;

        let mut prost_config = prost_build::Config::new();
        prost_config.out_dir(&temp_dir);
        if let Err(e) = prost_config.compile_protos(&[&proto_path], &[ctx.output_root]) {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(crate::templates::TemplateError::ConfigMismatch(
                format!("prost-build failed for '{}': {e} (is protoc installed?)", config.schema_file)
            ));
        }

        // prost-build names files after the package, not the input file.
        // Collect all generated .rs files and concatenate them into a single module.
        let mut generated_sources = Vec::new();
        for entry in std::fs::read_dir(&temp_dir).map_err(|e| crate::templates::TemplateError::ConfigMismatch(
            format!("failed to read prost-build output: {e}")
        ))? {
            let entry = entry.map_err(|e| crate::templates::TemplateError::ConfigMismatch(
                format!("failed to read prost-build output entry: {e}")
            ))?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                let content = std::fs::read_to_string(&path).map_err(|e| crate::templates::TemplateError::ConfigMismatch(
                    format!("failed to read generated protobuf Rust file: {e}")
                ))?;
                generated_sources.push(content);
            }
        }

        let _ = std::fs::remove_dir_all(&temp_dir);

        if generated_sources.is_empty() {
            return Err(crate::templates::TemplateError::ConfigMismatch(
                format!("prost-build produced no Rust files for '{}'", config.schema_file)
            ));
        }

        let raw_source = generated_sources.join("\n\n");
        let source = crate::codegen::format::validate_and_format(
            &raw_source,
            self.id().as_str(),
            &ctx.node.id.0,
        )
        .map_err(|e| crate::templates::TemplateError::ConfigMismatch(
            format!("prost-build emitted malformed Rust for '{}': {e}", config.schema_file)
        ))?;

        Ok(crate::templates::codegen::SchemaEmission {
            items: vec![crate::templates::codegen::EmittedItem {
                module_path: format!("parsers/{}.rs", to_snake_case(&name)),
                source,
            }],
            dependencies: vec![("prost".to_string(), "0.14".to_string())],
        })
    }
}

// ---- integration.consumer.placeholder -------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
struct ConsumerConfig {
    /// Kafka topic name.
    topic: String,
    /// Consumer group name.
    group: String,
    /// Snake_case module name for the generated consumer. Defaults to the
    /// topic name with dashes replaced by underscores.
    #[serde(default)]
    name: Option<String>,
}

pub struct IntegrationConsumerPlaceholder {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl IntegrationConsumerPlaceholder {
    fn new() -> Self {
        Self {
            id: id_or_panic("integration.consumer.placeholder"),
            display: TemplateDisplay::new(
                "Kafka Consumer",
                "Integration",
                "Long-running Kafka consumer. Emits a loop stub in src/consumers/; real rdkafka integration lands in a later section.",
            ),
            inputs: vec![PortSpec::single(
                "entry",
                "any",
                "Application entry — wire from core.entry_point to show this consumer is spawned at startup.",
            )],
            outputs: vec![PortSpec::single("message", "bytes", "Raw message payload per delivery.")],
            schema: schema_value::<ConsumerConfig>(),
        }
    }
}

impl NodeTemplate for IntegrationConsumerPlaceholder {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(
        &self,
        ctx: &crate::templates::codegen::CodegenCtx<'_>,
    ) -> Result<crate::templates::codegen::RuntimeEmission, crate::templates::TemplateError> {
        let config: ConsumerConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| crate::templates::TemplateError::ConfigMismatch(e.to_string()))?;

        let name = config.name.unwrap_or_else(|| config.topic.replace('-', "_"));

        let source = format!(
            "use tracing::{{info, warn}};\n\npub async fn run() {{\n    let topic = \"{}\";\n    let group = \"{}\";\n    info!(%topic, %group, \"consumer starting\");\n    loop {{\n        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;\n        info!(%topic, \"poll placeholder\");\n    }}\n}}\n",
            config.topic, config.group
        );

        Ok(crate::templates::codegen::RuntimeEmission {
            items: vec![crate::templates::codegen::EmittedItem {
                module_path: format!("consumers/{}.rs", name),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---- integration.scheduler.placeholder ------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
struct SchedulerConfig {
    /// Cron expression (5-field standard).
    cron: String,
    /// Snake_case module name for the generated scheduler. Defaults to
    /// "scheduler".
    #[serde(default)]
    name: Option<String>,
}

pub struct IntegrationSchedulerPlaceholder {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl IntegrationSchedulerPlaceholder {
    fn new() -> Self {
        Self {
            id: id_or_panic("integration.scheduler.placeholder"),
            display: TemplateDisplay::new(
                "Scheduler",
                "Integration",
                "Cron-driven trigger. Emits a loop stub in src/schedulers/; real cron integration lands in a later section.",
            ),
            inputs: vec![PortSpec::single(
                "entry",
                "any",
                "Application entry — wire from core.entry_point to show this scheduler is spawned at startup.",
            )],
            outputs: vec![PortSpec::single("tick", "tick", "Empty tick per scheduled firing.")],
            schema: schema_value::<SchedulerConfig>(),
        }
    }
}

impl NodeTemplate for IntegrationSchedulerPlaceholder {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(
        &self,
        ctx: &crate::templates::codegen::CodegenCtx<'_>,
    ) -> Result<crate::templates::codegen::RuntimeEmission, crate::templates::TemplateError> {
        let config: SchedulerConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| crate::templates::TemplateError::ConfigMismatch(e.to_string()))?;

        let name = config.name.unwrap_or_else(|| "scheduler".to_string());

        let source = format!(
            "use tracing::{{info, warn}};\n\npub async fn run() {{\n    let cron = \"{}\";\n    info!(%cron, \"scheduler starting\");\n    loop {{\n        tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;\n        info!(%cron, \"tick placeholder\");\n    }}\n}}\n",
            config.cron
        );

        Ok(crate::templates::codegen::RuntimeEmission {
            items: vec![crate::templates::codegen::EmittedItem {
                module_path: format!("schedulers/{}.rs", name),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_with_builtins_registers_expected_inventory() {
        // Pin-test of the builtin inventory. Adding a template means adding
        // its id here in sorted order and bumping the len assertion — the
        // failure surface is intentional so any new registration is a
        // deliberate, reviewed change.
        let r = TemplateRegistry::with_builtins();
        let ids: Vec<_> = r.summaries().into_iter().map(|s| s.id.as_str().to_string()).collect();
        assert_eq!(
            ids,
            vec![
                "core.dto",
                "core.entry_point",
                "core.service",
                "custom.block",
                "http.handler",
                "http.route",
                "integration.consumer.placeholder",
                "integration.db_writer",
                "integration.file_tail",
                "integration.http_client",
                "integration.kafka_consumer",
                "integration.kafka_producer",
                "integration.redis",
                "integration.scheduler",
                "integration.scheduler.placeholder",
                "integration.sql_connector",
                "language.await",
                "language.clone",
                "language.enum",
                "language.fn",
                "language.if_else",
                "language.loop",
                "language.match",
                "language.pointer",
                "language.propagate",
                "language.struct",
                "observability.logger",
                "parser.json",
                "parser.protobuf",
                "parser.xml",
                "stream.filter",
                "stream.join",
                "stream.map",
                "stream.pattern",
                "stream.select",
                "stream.union",
                "stream.window",
                "tokio.broadcast",
                "tokio.interval",
                "tokio.join",
                "tokio.mpsc",
                "tokio.mutex",
                "tokio.notify",
                "tokio.rwlock",
                "tokio.select",
                "tokio.semaphore",
                "tokio.sleep",
                "tokio.spawn",
                "tokio.spawn_blocking",
            ],
            "expected exact set of builtin ids (sorted lexicographically by summaries())"
        );
        assert_eq!(r.len(), 49);
    }

    #[test]
    fn test_http_route_accepts_valid_config_and_rejects_bad_method() {
        let r = TemplateRegistry::with_builtins();
        let id = TemplateId::new("http.route").unwrap();
        // Valid
        assert!(r.validate(&id, &json!({"path": "/hello", "method": "GET"})).is_ok());
        // Bad method (lowercase) — schema enum is UPPERCASE
        assert!(r.validate(&id, &json!({"path": "/", "method": "get"})).is_err());
        // Missing required field
        assert!(r.validate(&id, &json!({"method": "GET"})).is_err());
    }

    #[test]
    fn test_core_dto_accepts_field_list() {
        let r = TemplateRegistry::with_builtins();
        let id = TemplateId::new("core.dto").unwrap();
        assert!(r
            .validate(
                &id,
                &json!({
                    "name": "User",
                    "fields": [{"name": "id", "ty": "u64"}, {"name": "email", "ty": "String"}]
                })
            )
            .is_ok());
        // Missing required `fields`
        assert!(r.validate(&id, &json!({"name": "User"})).is_err());
    }

    #[test]
    fn test_observability_logger_codegen_mode_is_runtime() {
        let r = TemplateRegistry::with_builtins();
        let id = TemplateId::new("observability.logger").unwrap();
        let t = r.get(&id).unwrap();
        assert_eq!(t.codegen_mode(), CodegenMode::Runtime);
        assert_eq!(t.debug_bridge(), DebugBridgeKind::PassThrough);
    }

    #[test]
    fn test_core_dto_codegen_mode_is_codegen() {
        let r = TemplateRegistry::with_builtins();
        let id = TemplateId::new("core.dto").unwrap();
        let t = r.get(&id).unwrap();
        assert_eq!(t.codegen_mode(), CodegenMode::Codegen);
    }

    #[test]
    fn test_long_runners_declare_long_runner_bridge() {
        let r = TemplateRegistry::with_builtins();
        for id_str in &[
            "integration.consumer.placeholder",
            "integration.scheduler.placeholder",
        ] {
            let id = TemplateId::new(id_str).unwrap();
            let t = r.get(&id).unwrap();
            assert_eq!(t.debug_bridge(), DebugBridgeKind::LongRunner, "for {id_str}");
        }
    }

    #[test]
    fn test_observability_logger_emits_real_code() {
        let r = TemplateRegistry::with_builtins();
        let id = TemplateId::new("observability.logger").unwrap();
        let t = r.get(&id).unwrap();

        // Build a minimal codegen context for the logger node.
        let node = crate::projects::types::Node {
            id: crate::projects::types::NodeId("n1".into()),
            template_id: id.clone(),
            position: crate::projects::types::Position { x: 0.0, y: 0.0 },
            config: serde_json::json!({"level": "info", "format": "pretty", "name": "request_logger"}),
            label: None,
        };
        let graph = crate::projects::types::Graph::default();
        let ctx = crate::templates::codegen::CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &std::path::PathBuf::from("/tmp"),
            graph: &graph,
        };

        let emission = t.emit_runtime(&ctx).expect("logger emits valid runtime code");
        assert!(!emission.is_placeholder(), "logger must not be a placeholder after S7");
        assert_eq!(emission.items.len(), 1);
        assert_eq!(emission.items[0].module_path, "loggers/request_logger.rs");
        assert!(emission.items[0].source.contains("tracing::info!"));
        assert!(emission.items[0].source.contains("pub fn record<T: std::fmt::Debug>"));
    }

    #[test]
    fn test_handler_without_upstream_emits_plain_ok() {
        let r = TemplateRegistry::with_builtins();
        let id = TemplateId::new("http.handler").unwrap();
        let t = r.get(&id).unwrap();

        let node = crate::projects::types::Node {
            id: crate::projects::types::NodeId("h1".into()),
            template_id: id.clone(),
            position: crate::projects::types::Position { x: 0.0, y: 0.0 },
            config: serde_json::json!({"name": "hello"}),
            label: None,
        };
        let graph = crate::projects::types::Graph::default();
        let ctx = crate::templates::codegen::CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &std::path::PathBuf::from("/tmp"),
            graph: &graph,
        };

        let emission = t.emit_runtime(&ctx).unwrap();
        let src = &emission.items[0].source;
        assert!(!src.contains("use crate::services"));
        assert!(src.contains("pub async fn hello() -> Result<impl IntoResponse, AppError>"));
        assert!(src.contains("use crate::errors::AppError"));
    }

    #[test]
    fn test_handler_wired_to_service_emits_use_and_call() {
        let r = TemplateRegistry::with_builtins();
        let id = TemplateId::new("http.handler").unwrap();
        let t = r.get(&id).unwrap();

        let svc_node = crate::projects::types::Node {
            id: crate::projects::types::NodeId("s1".into()),
            template_id: TemplateId::new("core.service").unwrap(),
            position: crate::projects::types::Position { x: 0.0, y: 0.0 },
            config: serde_json::json!({"name": "get_user"}),
            label: None,
        };
        let handler_node = crate::projects::types::Node {
            id: crate::projects::types::NodeId("h1".into()),
            template_id: id.clone(),
            position: crate::projects::types::Position { x: 0.0, y: 0.0 },
            config: serde_json::json!({"name": "hello"}),
            label: None,
        };
        let graph = crate::projects::types::Graph {
            schema_version: crate::projects::types::GRAPH_SCHEMA_VERSION,
            nodes: vec![svc_node.clone(), handler_node.clone()],
            edges: vec![crate::projects::types::Edge {
                id: crate::projects::types::EdgeId("e1".into()),
                source: svc_node.id.clone(),
                target: handler_node.id.clone(),
                source_port: "output".into(),
                target_port: "request".into(),
            }],
        };
        let ctx = crate::templates::codegen::CodegenCtx {
            project_slug: "test",
            node: &handler_node,
            output_root: &std::path::PathBuf::from("/tmp"),
            graph: &graph,
        };

        let emission = t.emit_runtime(&ctx).unwrap();
        let src = &emission.items[0].source;
        assert!(src.contains("use crate::services::get_user;"));
        assert!(src.contains("let _ = get_user::get_user().await?;"));
        assert!(src.contains("pub async fn hello() -> Result<impl IntoResponse, AppError>"));
        assert!(src.contains("use crate::errors::AppError"));
    }

    #[test]
    fn test_service_wired_to_dto_emits_use_and_type_ref() {
        let r = TemplateRegistry::with_builtins();
        let id = TemplateId::new("core.service").unwrap();
        let t = r.get(&id).unwrap();

        let dto_node = crate::projects::types::Node {
            id: crate::projects::types::NodeId("d1".into()),
            template_id: TemplateId::new("core.dto").unwrap(),
            position: crate::projects::types::Position { x: 0.0, y: 0.0 },
            config: serde_json::json!({"name": "User", "fields": [{"name": "id", "ty": "u64"}]}),
            label: None,
        };
        let svc_node = crate::projects::types::Node {
            id: crate::projects::types::NodeId("s1".into()),
            template_id: id.clone(),
            position: crate::projects::types::Position { x: 0.0, y: 0.0 },
            config: serde_json::json!({"name": "get_user"}),
            label: None,
        };
        let graph = crate::projects::types::Graph {
            schema_version: crate::projects::types::GRAPH_SCHEMA_VERSION,
            nodes: vec![dto_node.clone(), svc_node.clone()],
            edges: vec![crate::projects::types::Edge {
                id: crate::projects::types::EdgeId("e1".into()),
                source: dto_node.id.clone(),
                target: svc_node.id.clone(),
                source_port: "output".into(),
                target_port: "input".into(),
            }],
        };
        let ctx = crate::templates::codegen::CodegenCtx {
            project_slug: "test",
            node: &svc_node,
            output_root: &std::path::PathBuf::from("/tmp"),
            graph: &graph,
        };

        let emission = t.emit_runtime(&ctx).unwrap();
        let src = &emission.items[0].source;
        assert!(src.contains("use crate::dto::user::User;"));
        assert!(src.contains("let _: Option<User> = None;"));
        assert!(src.contains("pub async fn get_user() -> Result<&'static str, AppError>"));
        assert!(src.contains("use crate::errors::AppError"));
    }

    #[test]
    fn test_service_without_upstream_emits_result_with_app_error() {
        let r = TemplateRegistry::with_builtins();
        let id = TemplateId::new("core.service").unwrap();
        let t = r.get(&id).unwrap();

        let svc_node = crate::projects::types::Node {
            id: crate::projects::types::NodeId("s1".into()),
            template_id: id.clone(),
            position: crate::projects::types::Position { x: 0.0, y: 0.0 },
            config: serde_json::json!({"name": "get_user"}),
            label: None,
        };
        let graph = crate::projects::types::Graph::default();
        let ctx = crate::templates::codegen::CodegenCtx {
            project_slug: "test",
            node: &svc_node,
            output_root: &std::path::PathBuf::from("/tmp"),
            graph: &graph,
        };

        let emission = t.emit_runtime(&ctx).unwrap();
        let src = &emission.items[0].source;
        assert!(!src.contains("use crate::dto"));
        assert!(src.contains("pub async fn get_user() -> Result<&'static str, AppError>"));
        assert!(src.contains("use crate::errors::AppError"));
        assert!(src.contains("Ok(\"ok\")"));
    }

    #[test]
    fn test_parser_json_generates_struct_from_schema() {
        let r = TemplateRegistry::with_builtins();
        let id = TemplateId::new("parser.json").unwrap();
        let t = r.get(&id).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let schema_path = dir.path().join("person.json");
        std::fs::write(
            &schema_path,
            r#"{"title":"Person","type":"object","properties":{"name":{"type":"string"},"age":{"type":"integer"}},"required":["name","age"]}"#,
        ).unwrap();

        let node = crate::projects::types::Node {
            id: crate::projects::types::NodeId("p1".into()),
            template_id: id.clone(),
            position: crate::projects::types::Position { x: 0.0, y: 0.0 },
            config: serde_json::json!({"schema_file": "person.json", "name": "person_parser"}),
            label: None,
        };
        let graph = crate::projects::types::Graph::default();
        let ctx = crate::templates::codegen::CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &dir.path().to_path_buf(),
            graph: &graph,
        };

        let emission = t.emit_schema(&ctx).unwrap();
        assert_eq!(emission.items.len(), 1);
        assert_eq!(emission.items[0].module_path, "parsers/person_parser.rs");
        let src = &emission.items[0].source;
        assert!(src.contains("struct Person"), "generated source must contain Person struct: {src}");
        assert!(syn::parse_file(src).is_ok(), "generated source must be valid Rust");
    }

    #[test]
    fn test_parser_protobuf_generates_struct_from_proto() {
        let r = TemplateRegistry::with_builtins();
        let id = TemplateId::new("parser.protobuf").unwrap();
        let t = r.get(&id).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let proto_path = dir.path().join("person.proto");
        std::fs::write(
            &proto_path,
            r#"syntax = "proto3";
package person;
message Person {
  string name = 1;
  int32 age = 2;
}
"#,
        ).unwrap();

        let node = crate::projects::types::Node {
            id: crate::projects::types::NodeId("p1".into()),
            template_id: id.clone(),
            position: crate::projects::types::Position { x: 0.0, y: 0.0 },
            config: serde_json::json!({"schema_file": "person.proto", "name": "person_parser"}),
            label: None,
        };
        let graph = crate::projects::types::Graph::default();
        let ctx = crate::templates::codegen::CodegenCtx {
            project_slug: "test",
            node: &node,
            output_root: &dir.path().to_path_buf(),
            graph: &graph,
        };

        let emission = t.emit_schema(&ctx).unwrap();
        assert_eq!(emission.items.len(), 1);
        assert_eq!(emission.items[0].module_path, "parsers/person_parser.rs");
        let src = &emission.items[0].source;
        assert!(src.contains("struct Person"), "generated source must contain Person struct: {src}");
        assert!(src.contains("::prost::Message"), "generated source must use prost::Message: {src}");
        assert!(emission.dependencies.contains(&("prost".to_string(), "0.14".to_string())));
        assert!(syn::parse_file(src).is_ok(), "generated source must be valid Rust");
    }
}
