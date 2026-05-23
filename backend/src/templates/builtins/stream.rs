//! Stream Processing & CEP Operators templates (S23–S26).
//!
//! Provides templates for:
//! - Unary operators: `stream.filter`, `stream.map`, `stream.select`
//! - Binary operators: `stream.union`, `stream.join`
//! - Windowing engine: `stream.window`
//! - CEP Sequence Pattern engine: `stream.pattern`

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::templates::{
    ports::PortSpec,
    codegen::{CodegenCtx, EmittedItem, RuntimeEmission},
    DebugBridgeKind, NodeTemplate, TemplateDisplay, TemplateError, TemplateId,
};
use super::{id_or_panic, schema_value, to_snake_case};

// ---- Downstream Call Generator Helper --------------------------------------

fn generate_downstream_calls(ctx: &CodegenCtx<'_>, source_port: &str, payload_expr: &str) -> (String, String) {
    let mut downstream_uses = String::new();
    let mut downstream_calls = String::new();

    for edge in &ctx.graph.edges {
        if edge.source == ctx.node.id && edge.source_port == source_port {
            if let Some(target) = ctx.graph.nodes.iter().find(|n| n.id == edge.target) {
                let target_template = target.template_id.as_str();
                if target_template == "core.service" {
                    let svc_name = target.config.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
                    downstream_uses.push_str(&format!("use crate::services::{};\n", svc_name));
                    downstream_calls.push_str(&format!("        let _ = {}::{}({}.clone()).await;\n", svc_name, svc_name, payload_expr));
                } else if target_template == "integration.db_writer" {
                    let db_name = target.config.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
                    downstream_uses.push_str(&format!("use crate::integrations::{};\n", db_name));
                    downstream_calls.push_str(&format!("        let _ = {}::execute(vec![{}.clone()]).await;\n", db_name, payload_expr));
                } else if target_template == "custom.block" {
                    let custom_name = target.config.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
                    downstream_uses.push_str(&format!("use crate::functions::{};\n", custom_name));
                    downstream_calls.push_str(&format!("        let _ = {}::{}({}.clone()).await;\n", custom_name, custom_name, payload_expr));
                } else if target_template.starts_with("stream.") {
                    let op_name = target.config.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
                    let snake_op = op_name.to_lowercase().replace('.', "_").replace('-', "_");
                    downstream_uses.push_str(&format!("use crate::streams::{};\n", snake_op));
                    if target_template == "stream.union" {
                        let method = if edge.target_port == "input1" { "send1" } else { "send2" };
                        downstream_calls.push_str(&format!("        let _ = {}::{}({}.clone()).await;\n", snake_op, method, payload_expr));
                    } else if target_template == "stream.join" {
                        let method = if edge.target_port == "left" { "send_left" } else { "send_right" };
                        downstream_calls.push_str(&format!("        let _ = {}::{}({}.clone()).await;\n", snake_op, method, payload_expr));
                    } else {
                        downstream_calls.push_str(&format!("        let _ = {}::send({}.clone()).await;\n", snake_op, payload_expr));
                    }
                }
            }
        }
    }
    (downstream_uses, downstream_calls)
}

// ---- stream.filter --------------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
pub struct StreamFilterConfig {
    /// Unique operator name.
    pub name: String,
    /// Predicate condition, e.g. `event.value > 100.0` or `event.status == "CRITICAL"`.
    pub predicate: String,
    /// The Rust type name of stream elements.
    pub item_type: String,
    /// Enable high-performance parallel execution (spawns tokio task per event).
    #[serde(default)]
    pub parallel: bool,
}

pub struct StreamFilter {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl StreamFilter {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("stream.filter"),
            display: TemplateDisplay::new(
                "Stream Filter",
                "Streaming",
                "Filters incoming events based on a boolean predicate condition.",
            ),
            inputs: vec![PortSpec::single("input", "any", "Incoming event stream.")],
            outputs: vec![PortSpec::single("output", "any", "Filtered event stream.")],
            schema: schema_value::<StreamFilterConfig>(),
        }
    }
}

impl NodeTemplate for StreamFilter {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: StreamFilterConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let op_name = to_snake_case(&config.name);
        let predicate = config.predicate;
        let item_type = config.item_type;

        let (downstream_uses, downstream_calls) = generate_downstream_calls(ctx, "output", "event");

        let loop_body = if config.parallel {
            format!(
                r#"        let event_clone = event.clone();
        tokio::spawn(async move {{
            let event = event_clone;
            if {predicate} {{
{downstream_calls}
            }}
        }});"#
            )
        } else {
            format!(
                r#"        if {predicate} {{
{downstream_calls}
        }}"#
            )
        };

        let source = format!(
            r#"use std::sync::{{OnceLock, Arc}};
use tracing::info;
{downstream_uses}

pub static SENDER: OnceLock<tokio::sync::mpsc::Sender<{item_type}>> = OnceLock::new();
pub static RECEIVER: OnceLock<Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{item_type}>>>> = OnceLock::new();

pub fn get_channel() -> (tokio::sync::mpsc::Sender<{item_type}>, Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{item_type}>>>) {{
    let rx_lock = RECEIVER.get_or_init(|| {{
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        let _ = SENDER.set(tx);
        Arc::new(tokio::sync::Mutex::new(rx))
    }});
    let sender = match SENDER.get() {{
        Some(s) => s.clone(),
        None => {{
            let (tx, _) = tokio::sync::mpsc::channel(1);
            tx
        }}
    }};
    (sender, rx_lock.clone())
}}

pub async fn send(event: {item_type}) {{
    let (tx, _) = get_channel();
    let _ = tx.send(event).await;
}}

pub async fn run() {{
    info!("Stream filter operator '{op_name}' started");
    let (_, rx) = get_channel();
    loop {{
        let event = {{
            let mut guard = rx.lock().await;
            guard.recv().await
        }};
        if let Some(event) = event {{
{loop_body}
        }} else {{
            break;
        }}
    }}
}}
"#
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("streams/{}.rs", op_name),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---- stream.map -----------------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
pub struct StreamMapConfig {
    /// Unique operator name.
    pub name: String,
    /// Rust mapping expression, e.g. `OutType { doubled: event.value * 2 }`.
    pub expression: String,
    /// The input Rust type name.
    pub input_type: String,
    /// The output Rust type name.
    pub output_type: String,
    /// Enable high-performance parallel execution.
    #[serde(default)]
    pub parallel: bool,
}

pub struct StreamMap {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl StreamMap {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("stream.map"),
            display: TemplateDisplay::new(
                "Stream Map",
                "Streaming",
                "Transforms incoming events using a custom mapping expression.",
            ),
            inputs: vec![PortSpec::single("input", "any", "Incoming event stream.")],
            outputs: vec![PortSpec::single("output", "any", "Mapped event stream.")],
            schema: schema_value::<StreamMapConfig>(),
        }
    }
}

impl NodeTemplate for StreamMap {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: StreamMapConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let op_name = to_snake_case(&config.name);
        let expression = config.expression;
        let input_type = config.input_type;
        let _output_type = config.output_type;

        let (downstream_uses, downstream_calls) = generate_downstream_calls(ctx, "output", "mapped");

        let loop_body = if config.parallel {
            format!(
                r#"        let event_clone = event.clone();
        tokio::spawn(async move {{
            let event = event_clone;
            let mapped = {expression};
{downstream_calls}
        }});"#
            )
        } else {
            format!(
                r#"        let mapped = {expression};
{downstream_calls}"#
            )
        };

        let source = format!(
            r#"use std::sync::{{OnceLock, Arc}};
use tracing::info;
{downstream_uses}

pub static SENDER: OnceLock<tokio::sync::mpsc::Sender<{input_type}>> = OnceLock::new();
pub static RECEIVER: OnceLock<Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{input_type}>>>> = OnceLock::new();

pub fn get_channel() -> (tokio::sync::mpsc::Sender<{input_type}>, Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{input_type}>>>) {{
    let rx_lock = RECEIVER.get_or_init(|| {{
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        let _ = SENDER.set(tx);
        Arc::new(tokio::sync::Mutex::new(rx))
    }});
    let sender = match SENDER.get() {{
        Some(s) => s.clone(),
        None => {{
            let (tx, _) = tokio::sync::mpsc::channel(1);
            tx
        }}
    }};
    (sender, rx_lock.clone())
}}

pub async fn send(event: {input_type}) {{
    let (tx, _) = get_channel();
    let _ = tx.send(event).await;
}}

pub async fn run() {{
    info!("Stream map operator '{op_name}' started");
    let (_, rx) = get_channel();
    loop {{
        let event = {{
            let mut guard = rx.lock().await;
            guard.recv().await
        }};
        if let Some(event) = event {{
{loop_body}
        }} else {{
            break;
        }}
    }}
}}
"#
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("streams/{}.rs", op_name),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---- stream.select --------------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
pub struct StreamSelectConfig {
    /// Unique operator name.
    pub name: String,
    /// Comma-separated list of fields to select from incoming struct.
    pub fields: String,
    /// The input Rust type name.
    pub input_type: String,
}

pub struct StreamSelect {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl StreamSelect {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("stream.select"),
            display: TemplateDisplay::new(
                "Stream Select",
                "Streaming",
                "Projects a subset of fields from a source struct into a dynamic new struct payload.",
            ),
            inputs: vec![PortSpec::single("input", "any", "Incoming event stream.")],
            outputs: vec![PortSpec::single("output", "any", "Selected projection event stream.")],
            schema: schema_value::<StreamSelectConfig>(),
        }
    }
}

impl NodeTemplate for StreamSelect {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: StreamSelectConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let op_name = to_snake_case(&config.name);
        let input_type = config.input_type;
        let camel_struct = config.name.replace('-', "_");
        let payload_struct = format!("{}Payload", camel_struct);

        let field_list: Vec<&str> = config.fields.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        let mut struct_fields = String::new();
        let mut field_mappings = String::new();

        for f in &field_list {
            struct_fields.push_str(&format!("    pub {}: serde_json::Value,\n", f));
            field_mappings.push_str(&format!("            {}: serde_json::to_value(&event.{}).unwrap_or(serde_json::Value::Null),\n", f, f));
        }

        let (downstream_uses, downstream_calls) = generate_downstream_calls(ctx, "output", "selected");

        let source = format!(
            r#"use std::sync::{{OnceLock, Arc}};
use tracing::info;
{downstream_uses}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct {payload_struct} {{
{struct_fields}}}

pub static SENDER: OnceLock<tokio::sync::mpsc::Sender<{input_type}>> = OnceLock::new();
pub static RECEIVER: OnceLock<Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{input_type}>>>> = OnceLock::new();

pub fn get_channel() -> (tokio::sync::mpsc::Sender<{input_type}>, Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{input_type}>>>) {{
    let rx_lock = RECEIVER.get_or_init(|| {{
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        let _ = SENDER.set(tx);
        Arc::new(tokio::sync::Mutex::new(rx))
    }});
    let sender = match SENDER.get() {{
        Some(s) => s.clone(),
        None => {{
            let (tx, _) = tokio::sync::mpsc::channel(1);
            tx
        }}
    }};
    (sender, rx_lock.clone())
}}

pub async fn send(event: {input_type}) {{
    let (tx, _) = get_channel();
    let _ = tx.send(event).await;
}}

pub async fn run() {{
    info!("Stream select operator '{op_name}' started");
    let (_, rx) = get_channel();
    loop {{
        let event = {{
            let mut guard = rx.lock().await;
            guard.recv().await
        }};
        if let Some(event) = event {{
            let selected = {payload_struct} {{
{field_mappings}            }};
{downstream_calls}        }} else {{
            break;
        }}
    }}
}}
"#
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("streams/{}.rs", op_name),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---- stream.union ---------------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
pub struct StreamUnionConfig {
    /// Unique operator name.
    pub name: String,
    /// The Rust type name of stream elements.
    pub item_type: String,
}

pub struct StreamUnion {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl StreamUnion {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("stream.union"),
            display: TemplateDisplay::new(
                "Stream Union",
                "Streaming",
                "Merges multiple incoming streams of compatible types into a single stream.",
            ),
            inputs: vec![
                PortSpec::single("input1", "any", "First stream input."),
                PortSpec::single("input2", "any", "Second stream input."),
            ],
            outputs: vec![PortSpec::single("output", "any", "Unified event stream.")],
            schema: schema_value::<StreamUnionConfig>(),
        }
    }
}

impl NodeTemplate for StreamUnion {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: StreamUnionConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let op_name = to_snake_case(&config.name);
        let item_type = config.item_type;

        let (downstream_uses, downstream_calls) = generate_downstream_calls(ctx, "output", "event");

        let source = format!(
            r#"use std::sync::{{OnceLock, Arc}};
use tracing::info;
{downstream_uses}

pub static SENDER1: OnceLock<tokio::sync::mpsc::Sender<{item_type}>> = OnceLock::new();
pub static SENDER2: OnceLock<tokio::sync::mpsc::Sender<{item_type}>> = OnceLock::new();
pub static RECEIVER1: OnceLock<Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{item_type}>>>> = OnceLock::new();
pub static RECEIVER2: OnceLock<Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{item_type}>>>> = OnceLock::new();

pub fn get_channels() -> (
    tokio::sync::mpsc::Sender<{item_type}>,
    tokio::sync::mpsc::Sender<{item_type}>,
    Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{item_type}>>>,
    Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{item_type}>>>,
) {{
    let rx_lock1 = RECEIVER1.get_or_init(|| {{
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        let _ = SENDER1.set(tx);
        Arc::new(tokio::sync::Mutex::new(rx))
    }});
    let rx_lock2 = RECEIVER2.get_or_init(|| {{
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        let _ = SENDER2.set(tx);
        Arc::new(tokio::sync::Mutex::new(rx))
    }});
    let sender1 = match SENDER1.get() {{
        Some(s) => s.clone(),
        None => {{
            let (tx, _) = tokio::sync::mpsc::channel(1);
            tx
        }}
    }};
    let sender2 = match SENDER2.get() {{
        Some(s) => s.clone(),
        None => {{
            let (tx, _) = tokio::sync::mpsc::channel(1);
            tx
        }}
    }};
    (
        sender1,
        sender2,
        rx_lock1.clone(),
        rx_lock2.clone(),
    )
}}

pub async fn send1(event: {item_type}) {{
    let (tx, _, _, _) = get_channels();
    let _ = tx.send(event).await;
}}

pub async fn send2(event: {item_type}) {{
    let (_, tx, _, _) = get_channels();
    let _ = tx.send(event).await;
}}

pub async fn run() {{
    info!("Stream union operator '{op_name}' started");
    let (_, _, rx1, rx2) = get_channels();
    loop {{
        let mut rx_guard1 = rx1.lock().await;
        let mut rx_guard2 = rx2.lock().await;
        tokio::select! {{
            Some(event) = rx_guard1.recv() => {{
{downstream_calls}            }}
            Some(event) = rx_guard2.recv() => {{
{downstream_calls}            }}
            else => break,
        }}
    }}
}}
"#
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("streams/{}.rs", op_name),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---- stream.join ----------------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
pub struct StreamJoinConfig {
    /// Unique operator name.
    pub name: String,
    /// Expression extracting left key, e.g. `left.id` or `left.order_id`.
    pub left_key: String,
    /// Expression extracting right key, e.g. `right.id` or `right.order_id`.
    pub right_key: String,
    /// Sliding time window duration in seconds.
    pub window_seconds: u32,
    /// Left input Rust type name.
    pub left_type: String,
    /// Right input Rust type name.
    pub right_type: String,
}

pub struct StreamJoin {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl StreamJoin {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("stream.join"),
            display: TemplateDisplay::new(
                "Stream Join",
                "Streaming",
                "Matches and correlates two streams based on matching key expressions within a sliding time window.",
            ),
            inputs: vec![
                PortSpec::single("left", "any", "Left event stream input."),
                PortSpec::single("right", "any", "Right event stream input."),
            ],
            outputs: vec![PortSpec::single("output", "any", "Correlated joined event stream.")],
            schema: schema_value::<StreamJoinConfig>(),
        }
    }
}

impl NodeTemplate for StreamJoin {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: StreamJoinConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let op_name = to_snake_case(&config.name);
        let left_key = config.left_key;
        let right_key = config.right_key;
        let window_seconds = config.window_seconds;
        let left_type = config.left_type;
        let right_type = config.right_type;
        let camel_struct = config.name.replace('-', "_");
        let joined_struct = format!("{}Joined", camel_struct);

        let (downstream_uses, downstream_calls) = generate_downstream_calls(ctx, "output", "joined");

        let source = format!(
            r#"use std::sync::{{OnceLock, Arc}};
use tracing::info;
{downstream_uses}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct {joined_struct} {{
    pub left: {left_type},
    pub right: {right_type},
}}

pub static SENDER_LEFT: OnceLock<tokio::sync::mpsc::Sender<{left_type}>> = OnceLock::new();
pub static SENDER_RIGHT: OnceLock<tokio::sync::mpsc::Sender<{right_type}>> = OnceLock::new();
pub static RECEIVER_LEFT: OnceLock<Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{left_type}>>>> = OnceLock::new();
pub static RECEIVER_RIGHT: OnceLock<Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{right_type}>>>> = OnceLock::new();

pub fn get_channels() -> (
    tokio::sync::mpsc::Sender<{left_type}>,
    tokio::sync::mpsc::Sender<{right_type}>,
    Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{left_type}>>>,
    Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{right_type}>>>,
) {{
    let rx_lock_l = RECEIVER_LEFT.get_or_init(|| {{
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        let _ = SENDER_LEFT.set(tx);
        Arc::new(tokio::sync::Mutex::new(rx))
    }});
    let rx_lock_r = RECEIVER_RIGHT.get_or_init(|| {{
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        let _ = SENDER_RIGHT.set(tx);
        Arc::new(tokio::sync::Mutex::new(rx))
    }});
    let sender_l = match SENDER_LEFT.get() {{
        Some(s) => s.clone(),
        None => {{
            let (tx, _) = tokio::sync::mpsc::channel(1);
            tx
        }}
    }};
    let sender_r = match SENDER_RIGHT.get() {{
        Some(s) => s.clone(),
        None => {{
            let (tx, _) = tokio::sync::mpsc::channel(1);
            tx
        }}
    }};
    (
        sender_l,
        sender_r,
        rx_lock_l.clone(),
        rx_lock_r.clone(),
    )
}}

pub async fn send_left(event: {left_type}) {{
    let (tx, _, _, _) = get_channels();
    let _ = tx.send(event).await;
}}

pub async fn send_right(event: {right_type}) {{
    let (_, tx, _, _) = get_channels();
    let _ = tx.send(event).await;
}}

pub async fn run() {{
    info!("Stream join operator '{op_name}' started");
    let (_, _, rx_left, rx_right) = get_channels();
    let mut left_buffer: Vec<(tokio::time::Instant, {left_type})> = Vec::new();
    let mut right_buffer: Vec<(tokio::time::Instant, {right_type})> = Vec::new();
    let window_dur = tokio::time::Duration::from_secs({window_seconds});

    loop {{
        let now = tokio::time::Instant::now();
        left_buffer.retain(|(t, _)| now.duration_since(*t) <= window_dur);
        right_buffer.retain(|(t, _)| now.duration_since(*t) <= window_dur);

        let mut rx_guard_l = rx_left.lock().await;
        let mut rx_guard_r = rx_right.lock().await;

        tokio::select! {{
            Some(left) = rx_guard_l.recv() => {{
                let l_now = tokio::time::Instant::now();
                let l_key = {{
                    let left = &left;
                    {left_key}.clone()
                }};

                for (_, right) in &right_buffer {{
                    let r_key = {{
                        let right = right;
                        {right_key}.clone()
                    }};
                    if l_key == r_key {{
                        let joined = {joined_struct} {{ left: left.clone(), right: right.clone() }};
{downstream_calls}                    }}
                }}
                left_buffer.push((l_now, left));
            }}
            Some(right) = rx_guard_r.recv() => {{
                let r_now = tokio::time::Instant::now();
                let r_key = {{
                    let right = &right;
                    {right_key}.clone()
                }};

                for (_, left) in &left_buffer {{
                    let l_key = {{
                        let left = left;
                        {left_key}.clone()
                    }};
                    if l_key == r_key {{
                        let joined = {joined_struct} {{ left: left.clone(), right: right.clone() }};
{downstream_calls}                    }}
                }}
                right_buffer.push((r_now, right));
            }}
            else => break,
        }}
    }}
}}
"#
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("streams/{}.rs", op_name),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---- stream.window --------------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
pub struct StreamWindowConfig {
    /// Unique operator name.
    pub name: String,
    /// Window style: Tumbling (non-overlapping) or Sliding (overlapping).
    pub window_type: String,
    /// Buffer triggers: Count (items) or Time (seconds).
    pub trigger_type: String,
    /// Count threshold or Time limit in seconds.
    pub trigger_value: usize,
    /// Action: COUNT, SUM, AVG, MIN, MAX.
    pub aggregation_fn: String,
    /// Struct member variable to accumulate, e.g. `event.value`.
    pub field_to_aggregate: String,
    /// Input Rust type name.
    pub input_type: String,
}

pub struct StreamWindow {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl StreamWindow {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("stream.window"),
            display: TemplateDisplay::new(
                "Stream Window",
                "Streaming",
                "Aggregates metrics over fixed or sliding intervals.",
            ),
            inputs: vec![PortSpec::single("input", "any", "Incoming event stream.")],
            outputs: vec![PortSpec::single("output", "any", "Aggregated scalar results stream.")],
            schema: schema_value::<StreamWindowConfig>(),
        }
    }
}

impl NodeTemplate for StreamWindow {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: StreamWindowConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let op_name = to_snake_case(&config.name);
        let window_type = config.window_type;
        let trigger_type = config.trigger_type;
        let trigger_value = config.trigger_value;
        let agg_fn = config.aggregation_fn;
        let field_to_aggregate = config.field_to_aggregate;
        let input_type = config.input_type;

        let (downstream_uses, downstream_calls) = generate_downstream_calls(ctx, "output", "result");

        let compute_aggregation = match agg_fn.as_str() {
            "COUNT" => "let result = buffer.len() as f64;".to_string(),
            "SUM" => format!("let result: f64 = buffer.iter().map(|event| {} as f64).sum();", field_to_aggregate),
            "AVG" => format!(
                r#"let sum: f64 = buffer.iter().map(|event| {} as f64).sum();
            let result = if buffer.is_empty() {{ 0.0 }} else {{ sum / (buffer.len() as f64) }};"#,
                field_to_aggregate
            ),
            "MIN" => format!(
                r#"let result = buffer.iter().map(|event| {} as f64).fold(f64::INFINITY, f64::min);"#,
                field_to_aggregate
            ),
            "MAX" => format!(
                r#"let result = buffer.iter().map(|event| {} as f64).fold(f64::NEG_INFINITY, f64::max);"#,
                field_to_aggregate
            ),
            _ => "let result = 0.0;".to_string(),
        };

        let handle_eviction = if window_type == "Sliding" {
            if trigger_type == "Count" {
                "if buffer.len() > limit { buffer.remove(0); }"
            } else {
                "buffer.retain(|(t, _)| now.duration_since(*t) <= window_dur);"
            }
        } else {
            "buffer.clear();"
        };

        let trigger_loop = if trigger_type == "Time" {
            format!(
                r#"    let mut buffer: Vec<(tokio::time::Instant, {input_type})> = Vec::new();
    let window_dur = tokio::time::Duration::from_secs({trigger_value});
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));

    loop {{
        let now = tokio::time::Instant::now();
        let mut rx_guard = rx.lock().await;
        tokio::select! {{
            Some(event) = rx_guard.recv() => {{
                buffer.push((tokio::time::Instant::now(), event));
            }}
            _ = interval.tick() => {{
                let active_events: Vec<&{input_type}> = buffer.iter()
                    .filter(|(t, _)| now.duration_since(*t) <= window_dur)
                    .map(|(_, e)| e)
                    .collect();
                
                if !active_events.is_empty() {{
                    let buffer = active_events;
                    {compute_aggregation}
{downstream_calls}                }}
                {handle_eviction}
            }}
            else => break,
        }}
    }}"#
            )
        } else {
            format!(
                r#"    let mut buffer: Vec<{input_type}> = Vec::new();
    let limit = {trigger_value}usize;

    loop {{
        let event = {{
            let mut guard = rx.lock().await;
            guard.recv().await
        }};
        if let Some(event) = event {{
            buffer.push(event);
            if buffer.len() >= limit {{
                {{
                    let buffer = &buffer;
                    {compute_aggregation}
{downstream_calls}                }}
                {handle_eviction}
            }}
        }} else {{
            break;
        }}
    }}"#
            )
        };

        let source = format!(
            r#"use std::sync::{{OnceLock, Arc}};
use tracing::info;
{downstream_uses}

pub static SENDER: OnceLock<tokio::sync::mpsc::Sender<{input_type}>> = OnceLock::new();
pub static RECEIVER: OnceLock<Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{input_type}>>>> = OnceLock::new();

pub fn get_channel() -> (tokio::sync::mpsc::Sender<{input_type}>, Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{input_type}>>>) {{
    let rx_lock = RECEIVER.get_or_init(|| {{
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        let _ = SENDER.set(tx);
        Arc::new(tokio::sync::Mutex::new(rx))
    }});
    let sender = match SENDER.get() {{
        Some(s) => s.clone(),
        None => {{
            let (tx, _) = tokio::sync::mpsc::channel(1);
            tx
        }}
    }};
    (sender, rx_lock.clone())
}}

pub async fn send(event: {input_type}) {{
    let (tx, _) = get_channel();
    let _ = tx.send(event).await;
}}

pub async fn run() {{
    info!("Stream window aggregation operator '{op_name}' started");
    let (_, rx) = get_channel();
{trigger_loop}
}}
"#
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("streams/{}.rs", op_name),
                source,
            }],
            dependencies: vec![],
            debug_site: None,
        })
    }
}

// ---- stream.pattern -------------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
pub struct StreamPatternConfig {
    /// Unique operator name.
    pub name: String,
    /// Matching predicate for A, e.g. `event.status == "CRITICAL"`.
    pub predicate_a: String,
    /// Matching predicate for B, e.g. `event.status == "RESOLVED"`.
    pub predicate_b: String,
    /// Time sequence window in seconds.
    pub window_seconds: u32,
    /// Input Rust type name.
    pub input_type: String,
}

pub struct StreamPattern {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl StreamPattern {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("stream.pattern"),
            display: TemplateDisplay::new(
                "CEP Pattern Matcher",
                "Streaming",
                "Lightweight DFA/NFA sequence pattern matcher (A followed by B within X seconds).",
            ),
            inputs: vec![PortSpec::single("input", "any", "Incoming event stream.")],
            outputs: vec![PortSpec::single("output", "any", "Pattern match notifications stream.")],
            schema: schema_value::<StreamPatternConfig>(),
        }
    }
}

impl NodeTemplate for StreamPattern {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: StreamPatternConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let op_name = to_snake_case(&config.name);
        let predicate_a = config.predicate_a;
        let predicate_b = config.predicate_b;
        let window_seconds = config.window_seconds;
        let input_type = config.input_type;
        let camel_struct = config.name.replace('-', "_");
        let match_struct = format!("{}Match", camel_struct);

        let (downstream_uses, downstream_calls) = generate_downstream_calls(ctx, "output", "matched");

        let source = format!(
            r#"use std::sync::{{OnceLock, Arc}};
use tracing::info;
{downstream_uses}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct {match_struct} {{
    pub event_a: {input_type},
    pub event_b: {input_type},
    pub matched_at: String,
}}

pub static SENDER: OnceLock<tokio::sync::mpsc::Sender<{input_type}>> = OnceLock::new();
pub static RECEIVER: OnceLock<Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{input_type}>>>> = OnceLock::new();

pub fn get_channel() -> (tokio::sync::mpsc::Sender<{input_type}>, Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<{input_type}>>>) {{
    let rx_lock = RECEIVER.get_or_init(|| {{
        let (tx, rx) = tokio::sync::mpsc::channel(1024);
        let _ = SENDER.set(tx);
        Arc::new(tokio::sync::Mutex::new(rx))
    }});
    let sender = match SENDER.get() {{
        Some(s) => s.clone(),
        None => {{
            let (tx, _) = tokio::sync::mpsc::channel(1);
            tx
        }}
    }};
    (sender, rx_lock.clone())
}}

pub async fn send(event: {input_type}) {{
    let (tx, _) = get_channel();
    let _ = tx.send(event).await;
}}

pub async fn run() {{
    info!("CEP pattern sequence operator '{op_name}' started");
    let (_, rx) = get_channel();
    let mut active_a: Option<(tokio::time::Instant, {input_type})> = None;
    let window_dur = tokio::time::Duration::from_secs({window_seconds});

    loop {{
        let event = {{
            let mut guard = rx.lock().await;
            guard.recv().await
        }};
        if let Some(event) = event {{
            let now = tokio::time::Instant::now();

            if let Some((started_at, _)) = active_a {{
                if now.duration_since(started_at) > window_dur {{
                    active_a = None;
                }}
            }}

            let is_a = {{
                let event = &event;
                {predicate_a}
            }};

            let is_b = {{
                let event = &event;
                {predicate_b}
            }};

            if is_a {{
                active_a = Some((now, event.clone()));
            }} else if is_b {{
                if let Some((started_at, prev_event)) = active_a.clone() {{
                    if now.duration_since(started_at) <= window_dur {{
                        let matched = {match_struct} {{
                            event_a: prev_event,
                            event_b: event.clone(),
                            matched_at: chrono::Utc::now().to_rfc3339(),
                        }};
                        active_a = None;
{downstream_calls}                    }}
                }}
            }}
        }} else {{
            break;
        }}
    }}
}}
"#
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("streams/{}.rs", op_name),
                source,
            }],
            dependencies: vec![("chrono".to_string(), "0.4".to_string())],
            debug_site: None,
        })
    }
}
