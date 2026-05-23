//! Ingest (Input) adapter templates for the generated user-projects.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::templates::{
    ports::PortSpec,
    DebugBridgeKind, NodeTemplate, TemplateDisplay, TemplateId,
};
use super::{id_or_panic, schema_value, to_snake_case};

// ---- integration.scheduler ------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
pub struct SchedulerConfig {
    /// Cron expression (5-field or 6-field standard).
    pub cron: String,
    /// Snake_case module name for the generated scheduler. Defaults to "scheduler".
    #[serde(default)]
    pub name: Option<String>,
}

pub struct IntegrationScheduler {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl IntegrationScheduler {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("integration.scheduler"),
            display: TemplateDisplay::new(
                "Cron Scheduler",
                "Integration",
                "Long-running cron-driven trigger. Parses and schedules ticks according to the cron expression.",
            ),
            inputs: vec![PortSpec::single(
                "entry",
                "any",
                "Application entry — wire from core.entry_point to spawn this scheduler at startup.",
            )],
            outputs: vec![PortSpec::single("tick", "tick", "Empty tick trigger per scheduled interval.")],
            schema: schema_value::<SchedulerConfig>(),
        }
    }
}

impl NodeTemplate for IntegrationScheduler {
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
        let cron_expr = config.cron;

        let mut downstream_uses = String::new();
        let mut downstream_calls = String::new();

        // Trace downstream nodes connected to the "tick" port.
        for edge in &ctx.graph.edges {
            if edge.source == ctx.node.id && edge.source_port == "tick" {
                if let Some(target) = ctx.graph.nodes.iter().find(|n| n.id == edge.target) {
                    if target.template_id.as_str() == "core.service" {
                        let svc_name = target.config.get("name").and_then(|v| v.as_str()).unwrap();
                        downstream_uses.push_str(&format!("use crate::services::{};\n", svc_name));
                        downstream_calls.push_str(&format!("        let _ = {}::{}().await;\n", svc_name, svc_name));
                    } else if target.template_id.as_str() == "integration.http_client" {
                        let client_name = target.config.get("name").and_then(|v| v.as_str()).unwrap();
                        downstream_uses.push_str(&format!("use crate::integrations::{};\n", client_name));
                        downstream_calls.push_str(&format!("        let _ = {}::send_request(\"tick\").await;\n", client_name));
                    } else if target.template_id.as_str() == "integration.db_writer" {
                        let db_name = target.config.get("name").and_then(|v| v.as_str()).unwrap();
                        downstream_uses.push_str(&format!("use crate::integrations::{};\n", db_name));
                        downstream_calls.push_str(&format!("        let _ = {}::execute(vec![]).await;\n", db_name));
                    }
                }
            }
        }

        let source = format!(
            r#"use std::str::FromStr;
use cron::Schedule;
use chrono::Local;
use tracing::{{info, error}};
{downstream_uses}
pub async fn run() {{
    let cron_str = "{cron_expr}";
    let schedule = match Schedule::from_str(cron_str) {{
        Ok(s) => s,
        Err(e) => {{
            error!(%cron_str, error = %e, "failed to parse cron expression; exiting task");
            return;
        }}
    }};
    info!(%cron_str, "scheduler starting");
    for datetime in schedule.upcoming(Local) {{
        let now = Local::now();
        if datetime > now {{
            if let Ok(duration) = (datetime - now).to_std() {{
                tokio::time::sleep(duration).await;
            }}
        }}
        info!("cron tick triggered");
{downstream_calls}    }}
}}
"#,
            cron_expr = cron_expr,
            downstream_uses = downstream_uses,
            downstream_calls = downstream_calls
        );

        Ok(crate::templates::codegen::RuntimeEmission {
            items: vec![crate::templates::codegen::EmittedItem {
                module_path: format!("schedulers/{}.rs", to_snake_case(&name)),
                source,
            }],
            dependencies: vec![
                ("cron".to_string(), "0.12".to_string()),
                ("chrono".to_string(), r#"{ version = "0.4", features = ["serde"] }"#.to_string()),
            ],
            debug_site: None,
        })
    }
}

// ---- integration.file_tail -------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
pub struct FileTailConfig {
    /// Path of the file to tail.
    pub file_path: String,
    /// Poll interval in milliseconds to check for new lines.
    #[serde(default = "default_poll_interval")]
    pub poll_interval_millis: u64,
    /// Snake_case module name. Defaults to "file_tail".
    #[serde(default)]
    pub name: Option<String>,
}

fn default_poll_interval() -> u64 { 500 }

pub struct IntegrationFileTail {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl IntegrationFileTail {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("integration.file_tail"),
            display: TemplateDisplay::new(
                "File Tail",
                "Integration",
                "Long-running file tail adapter. Reads new lines appended to a local file in real time.",
            ),
            inputs: vec![PortSpec::single(
                "entry",
                "any",
                "Application entry — wire from core.entry_point to spawn this file tail at startup.",
            )],
            outputs: vec![PortSpec::single("line", "string", "Raw line read from the tailed file.")],
            schema: schema_value::<FileTailConfig>(),
        }
    }
}

impl NodeTemplate for IntegrationFileTail {
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
        let config: FileTailConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| crate::templates::TemplateError::ConfigMismatch(e.to_string()))?;

        let name = config.name.unwrap_or_else(|| "file_tail".to_string());
        let file_path = config.file_path;
        let poll_ms = config.poll_interval_millis;

        let mut downstream_uses = String::new();
        let mut downstream_calls = String::new();

        // Trace downstream nodes connected to the "line" port.
        for edge in &ctx.graph.edges {
            if edge.source == ctx.node.id && edge.source_port == "line" {
                if let Some(target) = ctx.graph.nodes.iter().find(|n| n.id == edge.target) {
                    if target.template_id.as_str() == "core.service" {
                        let svc_name = target.config.get("name").and_then(|v| v.as_str()).unwrap();
                        downstream_uses.push_str(&format!("use crate::services::{};\n", svc_name));
                        downstream_calls.push_str(&format!("        let _ = {}::{}().await;\n", svc_name, svc_name));
                    } else if target.template_id.as_str() == "integration.http_client" {
                        let client_name = target.config.get("name").and_then(|v| v.as_str()).unwrap();
                        downstream_uses.push_str(&format!("use crate::integrations::{};\n", client_name));
                        downstream_calls.push_str(&format!("        let _ = {}::send_request(&line).await;\n", client_name));
                    } else if target.template_id.as_str() == "integration.db_writer" {
                        let db_name = target.config.get("name").and_then(|v| v.as_str()).unwrap();
                        downstream_uses.push_str(&format!("use crate::integrations::{};\n", db_name));
                        downstream_calls.push_str(&format!("        let _ = {}::execute(vec![line.clone()]).await;\n", db_name));
                    }
                }
            }
        }

        let source = format!(
            r#"use std::io::{{Seek, SeekFrom}};
use tokio::fs::File;
use tokio::io::{{AsyncBufReadExt, BufReader}};
use tracing::{{info, error, warn}};
{downstream_uses}
pub async fn run() {{
    let path = "{file_path}";
    info!(%path, "file tail starting");
    
    let mut file = match File::open(path).await {{
        Ok(f) => f,
        Err(e) => {{
            error!(%path, error = %e, "failed to open file for tailing; exiting task");
            return;
        }}
    }};

    // Seek to the end of the file at startup so we only read new lines.
    let mut std_file = file.into_std().await;
    let _ = std_file.seek(SeekFrom::End(0));
    let std_file = File::from_std(std_file);

    let reader = BufReader::new(std_file);
    let mut lines = reader.lines();

    loop {{
        match lines.next_line().await {{
            Ok(Some(line)) => {{
                info!(%line, "read line from file");
{downstream_calls}            }}
            Ok(None) => {{
                // End of file; wait a bit before checking again.
                tokio::time::sleep(tokio::time::Duration::from_millis({poll_ms})).await;
            }}
            Err(e) => {{
                error!(error = %e, "error reading tailed line; continuing");
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            }}
        }}
    }}
}}
"#,
            file_path = file_path,
            poll_ms = poll_ms,
            downstream_uses = downstream_uses,
            downstream_calls = downstream_calls
        );

        Ok(crate::templates::codegen::RuntimeEmission {
            items: vec![crate::templates::codegen::EmittedItem {
                module_path: format!("consumers/{}.rs", to_snake_case(&name)),
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
    use crate::templates::codegen::CodegenCtx;
    use crate::projects::types::{Graph, Node, NodeId, Position, GRAPH_SCHEMA_VERSION};
    use crate::templates::TemplateRegistry;
    use serde_json::json;
    use std::path::PathBuf;

    fn test_ctx(template_id: &str, config: Value) -> (PathBuf, Graph, Node) {
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
        (PathBuf::from("/tmp"), graph, node)
    }

    #[test]
    fn test_scheduler_validation() {
        let registry = TemplateRegistry::with_builtins();
        let id = TemplateId::new("integration.scheduler").unwrap();
        
        // Happy path
        assert!(registry.validate(&id, &json!({"cron": "0 * * * * *", "name": "my_job"})).is_ok());
        
        // Missing required field "cron"
        assert!(registry.validate(&id, &json!({"name": "job"})).is_err());
    }

    #[test]
    fn test_file_tail_validation() {
        let registry = TemplateRegistry::with_builtins();
        let id = TemplateId::new("integration.file_tail").unwrap();
        
        // Happy path
        assert!(registry.validate(&id, &json!({"file_path": "/var/log/syslog"})).is_ok());
        
        // Invalid poll interval type (expected number)
        assert!(registry.validate(&id, &json!({"file_path": "a.txt", "poll_interval_millis": "slow"})).is_err());
    }

    #[test]
    fn test_scheduler_emission_parses_as_rust() {
        let (root, graph, node) = test_ctx("integration.scheduler", json!({"cron": "0 * * * * *", "name": "scheduler"}));
        let template = IntegrationScheduler::new();
        let emission = template.emit_runtime(&CodegenCtx {
            project_slug: "test_proj",
            node: &node,
            output_root: &root,
            graph: &graph,
        }).unwrap();

        assert_eq!(emission.items.len(), 1);
        let item = &emission.items[0];
        assert_eq!(item.module_path, "schedulers/scheduler.rs");
        assert!(syn::parse_file(&item.source).is_ok(), "scheduler source must be valid Rust");
    }

    #[test]
    fn test_file_tail_emission_parses_as_rust() {
        let (root, graph, node) = test_ctx("integration.file_tail", json!({"file_path": "test.log", "name": "log_tailer"}));
        let template = IntegrationFileTail::new();
        let emission = template.emit_runtime(&CodegenCtx {
            project_slug: "test_proj",
            node: &node,
            output_root: &root,
            graph: &graph,
        }).unwrap();

        assert_eq!(emission.items.len(), 1);
        let item = &emission.items[0];
        assert_eq!(item.module_path, "consumers/log_tailer.rs");
        assert!(syn::parse_file(&item.source).is_ok(), "file_tail source must be valid Rust");
    }
}
