//! gRPC Server & Client visual node templates (Milestone 4 SDK).
//!
//! Emits tonic-based host-execution gRPC services and clients, compiling
//! Proto3 definitions dynamically at build-time using tonic-build in build.rs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::templates::{
    ports::PortSpec,
    codegen::{CodegenCtx, EmittedItem, RuntimeEmission},
    DebugBridgeKind, NodeTemplate, TemplateDisplay, TemplateError, TemplateId,
};
use super::{id_or_panic, schema_value, to_snake_case};

/// gRPC Server visual config payload.
#[derive(Debug, JsonSchema, Deserialize, Serialize, Clone)]
pub struct GrpcServerConfig {
    /// Port/Address to bind the gRPC Server listener, e.g. `[::1]:50051`.
    pub address: String,
    /// Raw Proto3 schema definition.
    pub proto_definition: String,
    /// Name of the declared service symbol to mount, e.g. `Greeter`.
    pub service_name: String,
    /// Dynamic inputs (managed by graph-save scanner).
    #[serde(default)]
    pub inputs: Vec<CustomPort>,
    /// Dynamic outputs (managed by graph-save scanner).
    #[serde(default)]
    pub outputs: Vec<CustomPort>,
}

/// gRPC Client visual config payload.
#[derive(Debug, JsonSchema, Deserialize, Serialize, Clone)]
pub struct GrpcClientConfig {
    /// gRPC Target URL to connect, e.g. `http://[::1]:50051`.
    pub address: String,
    /// Raw Proto3 schema definition.
    pub proto_definition: String,
    /// Name of the target service symbol, e.g. `Greeter`.
    pub service_name: String,
    /// Name of the RPC method to invoke, e.g. `SayHello`.
    pub method_name: String,
}

#[derive(Debug, JsonSchema, Deserialize, Serialize, Clone)]
pub struct CustomPort {
    pub name: String,
    pub ty: String,
}

/// Dynamic RPC service method descriptor.
#[derive(Debug, Clone)]
struct RpcMethod {
    name: String,
    req: String,
    res: String,
}

/// Visual gRPC Server template.
pub struct GrpcServer {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl GrpcServer {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("grpc.server"),
            display: TemplateDisplay::new(
                "gRPC Server",
                "gRPC",
                "High-performance gRPC Server listening on a TCP port. Auto-compiles Proto3 definitions and routes visual requests.",
            ),
            inputs: vec![],
            outputs: vec![],
            schema: schema_value::<GrpcServerConfig>(),
        }
    }
}

/// Visual gRPC Client template.
pub struct GrpcClient {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl GrpcClient {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("grpc.client"),
            display: TemplateDisplay::new(
                "gRPC Client",
                "gRPC",
                "Invokes an RPC method on an external gRPC Server.",
            ),
            inputs: vec![PortSpec::single("request", "any", "gRPC request payload.")],
            outputs: vec![PortSpec::single("response", "any", "gRPC response payload.")],
            schema: schema_value::<GrpcClientConfig>(),
        }
    }
}

impl NodeTemplate for GrpcServer {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(
        &self,
        ctx: &CodegenCtx<'_>,
    ) -> Result<RuntimeEmission, TemplateError> {
        let config: GrpcServerConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let address = config.address.trim();
        if address.is_empty() {
            return Err(TemplateError::SchemaInvalid("address cannot be empty".to_string()));
        }

        let service_name = config.service_name.trim();
        if service_name.is_empty() {
            return Err(TemplateError::SchemaInvalid("service_name cannot be empty".to_string()));
        }

        let package_name = parse_package_name(&config.proto_definition);
        let rpc_methods = parse_rpc_methods(&config.proto_definition);

        let node_id_snake = ctx.node.id.0.replace('-', "_");
        let proto_filename = format!("grpc_server_{}.proto", node_id_snake);

        // Generate the Tonic Service implementation
        let mut rpc_uses = String::new();
        let mut rpc_impls = String::new();

        for rpc in &rpc_methods {
            let target_node = ctx.graph.edges.iter()
                .find(|e| e.source == ctx.node.id && e.source_port == rpc.name)
                .map(|e| &e.target);

            let mut body = format!("Err(tonic::Status::unimplemented(\"Method {} is not visually connected\"))", rpc.name);

            if let Some(target_id) = target_node {
                if let Some(target) = ctx.graph.nodes.iter().find(|n| &n.id == target_id) {
                    if target.template_id.as_str() == "language.fn" {
                        if let Some(name_val) = target.config.get("name").and_then(|v| v.as_str()) {
                            rpc_uses.push_str(&format!("use crate::functions::{}::{};\n", name_val, name_val));
                            body = format!(
                                "let res = {}(req).await.map_err(|e| tonic::Status::internal(e.to_string()))?;\n        Ok(tonic::Response::new(res))",
                                name_val
                            );
                        }
                    } else if target.template_id.as_str() == "custom.block" {
                        if let Some(name_val) = target.config.get("name").and_then(|v| v.as_str()) {
                            let snake = to_snake_case(name_val);
                            rpc_uses.push_str(&format!("use crate::functions::{}::execute as execute_{};\n", snake, snake));
                            body = format!(
                                "let res = execute_{}(req).await.map_err(|e| tonic::Status::internal(e.to_string()))?;\n        Ok(tonic::Response::new(res))",
                                snake
                            );
                        }
                    }
                }
            }

            let snake_method = to_snake_case(&rpc.name);
            rpc_impls.push_str(&format!(
                r#"
    async fn {snake_method}(
        &self,
        request: tonic::Request<proto_{node_id_snake}::{}>,
    ) -> Result<tonic::Response<proto_{node_id_snake}::{}>, tonic::Status> {{
        let req = request.into_inner();
        {body}
    }}
"#,
                rpc.req, rpc.res,
            ));
        }

        let trait_service_snake = to_snake_case(service_name);
        let service_module = format!(
            r#"//! gRPC server task runner module.
//! Automatically compiled and managed by rust_no_code visual builder.

pub mod proto_{node_id_snake} {{
    tonic::include_proto!("{package_name}");
}}

{rpc_uses}
use tonic::{{transport::Server, Request, Response, Status}};
use proto_{node_id_snake}::{trait_service_snake}_server::{{{service_name}Server, {service_name}}};

#[derive(Debug, Default)]
pub struct My{service_name} {{}}

#[tonic::async_trait]
impl {service_name} for My{service_name} {{
{rpc_impls}
}}

/// background runner task
pub async fn run() {{
    let addr = match "{address}".parse() {{
        Ok(a) => a,
        Err(e) => {{
            tracing::error!("failed to parse bind address: {{}}", e);
            return;
        }}
    }};
    let service = My{service_name}::default();
    
    tracing::info!("gRPC server listening on {{}}", addr);
    if let Err(e) = Server::builder()
        .add_service({service_name}Server::new(service))
        .serve(addr)
        .await
    {{
        tracing::error!("gRPC Server runner crashed: {{}}", e);
    }}
}}
"#
        );

        let build_script = build_rs_scaffold();

        Ok(RuntimeEmission {
            items: vec![
                EmittedItem {
                    module_path: format!("grpc/server_{}.rs", node_id_snake),
                    source: service_module,
                },
                EmittedItem {
                    module_path: format!("../proto/{}", proto_filename),
                    source: config.proto_definition.clone(),
                },
                EmittedItem {
                    module_path: "../build.rs".to_string(),
                    source: build_script,
                },
            ],
            dependencies: vec![
                ("tonic".to_string(), r#"{ version = "0.10", features = ["tls", "codegen"] }"#.to_string()),
                ("prost".to_string(), "\"0.11\"".to_string()),
            ],
            debug_site: None,
        })
    }
}

impl NodeTemplate for GrpcClient {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::Default }

    fn emit_runtime(
        &self,
        ctx: &CodegenCtx<'_>,
    ) -> Result<RuntimeEmission, TemplateError> {
        let config: GrpcClientConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let address = config.address.trim();
        if address.is_empty() {
            return Err(TemplateError::SchemaInvalid("address cannot be empty".to_string()));
        }

        let service_name = config.service_name.trim();
        if service_name.is_empty() {
            return Err(TemplateError::SchemaInvalid("service_name cannot be empty".to_string()));
        }

        let method_name = config.method_name.trim();
        if method_name.is_empty() {
            return Err(TemplateError::SchemaInvalid("method_name cannot be empty".to_string()));
        }

        let package_name = parse_package_name(&config.proto_definition);
        let rpc_methods = parse_rpc_methods(&config.proto_definition);
        let active_rpc = rpc_methods.iter().find(|r| r.name == method_name)
            .ok_or_else(|| TemplateError::SchemaInvalid(format!("method_name '{}' not declared in Proto3 definition", method_name)))?;

        let node_id_snake = ctx.node.id.0.replace('-', "_");
        let proto_filename = format!("grpc_client_{}.proto", node_id_snake);

        let service_snake = to_snake_case(service_name);
        let method_snake = to_snake_case(method_name);

        let client_module = format!(
            r#"//! gRPC client executor module.
//! Automatically compiled and managed by rust_no_code visual builder.

pub mod proto_{node_id_snake} {{
    tonic::include_proto!("{package_name}");
}}

use proto_{node_id_snake}::{service_snake}_client::{service_name}Client;

/// Call the external gRPC RPC method `{method_name}` on `{address}`.
pub async fn execute(req: proto_{node_id_snake}::{}) -> Result<proto_{node_id_snake}::{}, anyhow::Error> {{
    let mut client = {service_name}Client::connect("{address}").await?;
    let response = client.{method_snake}(req).await?;
    Ok(response.into_inner())
}}
"#,
            active_rpc.req, active_rpc.res
        );

        let build_script = build_rs_scaffold();

        Ok(RuntimeEmission {
            items: vec![
                EmittedItem {
                    module_path: format!("grpc/client_{}.rs", node_id_snake),
                    source: client_module,
                },
                EmittedItem {
                    module_path: format!("../proto/{}", proto_filename),
                    source: config.proto_definition.clone(),
                },
                EmittedItem {
                    module_path: "../build.rs".to_string(),
                    source: build_script,
                },
            ],
            dependencies: vec![
                ("tonic".to_string(), r#"{ version = "0.10", features = ["tls", "codegen"] }"#.to_string()),
                ("prost".to_string(), "\"0.11\"".to_string()),
            ],
            debug_site: None,
        })
    }
}

/// Generic build.rs scaffolding to dynamically discover and compile all visual proto structures.
fn build_rs_scaffold() -> String {
    r#"// Generated by rust_no_code visual builder.
// Compiles all Proto3 definitions in the proto/ folder.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut proto_files = Vec::new();
    if let Ok(entries) = std::fs::read_dir("proto") {
        for entry in entries {
            if let Ok(entry) = entry {
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "proto") {
                    proto_files.push(path);
                }
            }
        }
    }
    if !proto_files.is_empty() {
        tonic_build::compile_protos(&proto_files, &["proto"])?;
    }
    Ok(())
}
"#.to_string()
}

/// Parse proto package name, defaulting to "hello".
fn parse_package_name(proto: &str) -> String {
    let re = regex::Regex::new(r"package\s+([a-zA-Z0-9_\.]+)\s*;").unwrap();
    if let Some(caps) = re.captures(proto) {
        caps.get(1).unwrap().as_str().to_string()
    } else {
        "hello".to_string()
    }
}

/// Parse all RPC method structures from proto definition using regex.
fn parse_rpc_methods(proto: &str) -> Vec<RpcMethod> {
    let mut out = Vec::new();
    let re = regex::Regex::new(r"(?x)
        rpc\s+
        (?P<name>[a-zA-Z0-9_]+)\s*
        \(\s*(?P<req>[a-zA-Z0-9_\.]+)\s*\)\s*
        returns\s*
        \(\s*(?P<res>[a-zA-Z0-9_\.]+)\s*\)
    ").unwrap();

    for caps in re.captures_iter(proto) {
        out.push(RpcMethod {
            name: caps.name("name").unwrap().as_str().to_string(),
            req: caps.name("req").unwrap().as_str().to_string(),
            res: caps.name("res").unwrap().as_str().to_string(),
        });
    }
    out
}

/// Core port extraction called on PUT graph validation pass.
pub fn parse_proto_ports(proto: &str) -> Result<(Vec<CustomPort>, Vec<CustomPort>), String> {
    let rpc_methods = parse_rpc_methods(proto);
    let mut outputs = Vec::new();

    for rpc in rpc_methods {
        outputs.push(CustomPort {
            name: rpc.name,
            ty: rpc.req,
        });
    }

    let inputs = vec![CustomPort {
        name: "entry".to_string(),
        ty: "any".to_string(),
    }];

    Ok((inputs, outputs))
}
