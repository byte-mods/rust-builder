//! Egress (Output) adapter templates for the generated user-projects.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::templates::{
    ports::PortSpec,
    NodeTemplate, TemplateDisplay, TemplateId,
};
use super::{id_or_panic, schema_value, to_snake_case};

// ---- integration.http_client ----------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
pub struct HttpClientConfig {
    /// Outbound URL endpoint.
    pub url: String,
    /// HTTP method to use.
    pub method: HttpMethod,
    /// Snake_case module name. Defaults to "http_client".
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, JsonSchema, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
}

pub struct IntegrationHttpClient {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl IntegrationHttpClient {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("integration.http_client"),
            display: TemplateDisplay::new(
                "HTTP Client",
                "Integration",
                "Sends outbound HTTP requests to a configured URL and returns the response body.",
            ),
            inputs: vec![PortSpec::single(
                "body",
                "string",
                "Request body to send (for POST requests; ignored for GET).",
            )],
            outputs: vec![PortSpec::single(
                "response",
                "string",
                "Outbound HTTP response body.",
            )],
            schema: schema_value::<HttpClientConfig>(),
        }
    }
}

impl NodeTemplate for IntegrationHttpClient {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }

    fn emit_runtime(
        &self,
        ctx: &crate::templates::codegen::CodegenCtx<'_>,
    ) -> Result<crate::templates::codegen::RuntimeEmission, crate::templates::TemplateError> {
        let config: HttpClientConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| crate::templates::TemplateError::ConfigMismatch(e.to_string()))?;

        let name = config.name.unwrap_or_else(|| "http_client".to_string());
        let url = config.url;
        let method_str = match config.method {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
        };

        let request_block = match config.method {
            HttpMethod::Get => {
                r#"let res = client.get(url).send().await;"#
            }
            HttpMethod::Post => {
                r#"let res = client.post(url).body(body.to_string()).send().await;"#
            }
        };

        let source = format!(
            r#"use crate::errors::AppError;
use tracing::{{info, error}};

pub async fn send_request(body: &str) -> Result<String, AppError> {{
    let url = "{url}";
    info!(method = "{method_str}", %url, "sending outbound HTTP request");
    
    let client = reqwest::Client::new();
    {request_block}

    match res {{
        Ok(response) => {{
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if status.is_success() {{
                info!(status = %status.as_u16(), "HTTP request succeeded");
                Ok(text)
            }} else {{
                error!(status = %status.as_u16(), %text, "HTTP request returned error status");
                Err(AppError::BadRequest(format!("HTTP error status: {{}}", status.as_u16())))
            }}
        }}
        Err(e) => {{
            error!(error = %e, "HTTP request connection failed");
            Err(AppError::Internal)
        }}
    }}
}}
"#,
            url = url,
            method_str = method_str,
            request_block = request_block
        );

        Ok(crate::templates::codegen::RuntimeEmission {
            items: vec![crate::templates::codegen::EmittedItem {
                module_path: format!("integrations/{}.rs", to_snake_case(&name)),
                source,
            }],
            dependencies: vec![
                ("reqwest".to_string(), r#"{ version = "0.12", features = ["json"] }"#.to_string()),
            ],
            debug_site: None,
        })
    }
}

// ---- integration.db_writer -------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize)]
#[allow(dead_code)]
pub struct DbWriterConfig {
    /// SQLite database file path.
    pub db_path: String,
    /// SQL insert / update query with ? parameter placeholders.
    pub query: String,
    /// Snake_case module name. Defaults to "db_writer".
    #[serde(default)]
    pub name: Option<String>,
}

pub struct IntegrationDbWriter {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl IntegrationDbWriter {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("integration.db_writer"),
            display: TemplateDisplay::new(
                "SQLite Writer",
                "Integration",
                "Executes SQL insert, update, or delete statements on an embedded SQLite database.",
            ),
            inputs: vec![PortSpec::single(
                "params",
                "any",
                "Parameters list (vector of strings) to bind to query placeholders.",
            )],
            outputs: vec![PortSpec::single(
                "result",
                "string",
                "Count of rows affected by the statement.",
            )],
            schema: schema_value::<DbWriterConfig>(),
        }
    }
}

impl NodeTemplate for IntegrationDbWriter {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }

    fn emit_runtime(
        &self,
        ctx: &crate::templates::codegen::CodegenCtx<'_>,
    ) -> Result<crate::templates::codegen::RuntimeEmission, crate::templates::TemplateError> {
        let config: DbWriterConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| crate::templates::TemplateError::ConfigMismatch(e.to_string()))?;

        let name = config.name.unwrap_or_else(|| "db_writer".to_string());
        let db_path = config.db_path;
        let query = config.query;

        let source = format!(
            r#"use crate::errors::AppError;
use tracing::{{info, error}};

pub async fn execute(params: Vec<String>) -> Result<u64, AppError> {{
    let db_path = "{db_path}";
    let query = "{query}";
    info!(%db_path, %query, "executing database query");

    let conn = match tokio_rusqlite::Connection::open(db_path).await {{
        Ok(c) => c,
        Err(e) => {{
            error!(error = %e, "failed to open database connection");
            return Err(AppError::Internal);
        }}
    }};

    let rows_affected = conn.call(move |conn| {{
        let mut stmt = conn.prepare(query)?;
        
        // Convert params list to refs for rusqlite statement execute.
        let params_refs: Vec<&dyn rusqlite::ToSql> = params
            .iter()
            .map(|s| s as &dyn rusqlite::ToSql)
            .collect();

        let count = stmt.execute(rusqlite::params_from_iter(params_refs))?;
        Ok(count as u64)
    }}).await;

    match rows_affected {{
        Ok(count) => {{
            info!(count, "database query executed successfully");
            Ok(count)
        }}
        Err(e) => {{
            error!(error = %e, "database query execution failed");
            Err(AppError::BadRequest(e.to_string()))
        }}
    }}
}}
"#,
            db_path = db_path,
            query = query
        );

        Ok(crate::templates::codegen::RuntimeEmission {
            items: vec![crate::templates::codegen::EmittedItem {
                module_path: format!("integrations/{}.rs", to_snake_case(&name)),
                source,
            }],
            dependencies: vec![
                ("tokio-rusqlite".to_string(), "0.6".to_string()),
                ("rusqlite".to_string(), r#"{ version = "0.32", features = ["bundled"] }"#.to_string()),
            ],
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
            comment: None,
        };
        let graph = Graph {
            schema_version: GRAPH_SCHEMA_VERSION,
            nodes: vec![node.clone()],
            edges: Vec::new(),
        };
        (PathBuf::from("/tmp"), graph, node)
    }

    #[test]
    fn test_http_client_validation() {
        let registry = TemplateRegistry::with_builtins();
        let id = TemplateId::new("integration.http_client").unwrap();
        
        // Happy path (GET)
        assert!(registry.validate(&id, &json!({"url": "http://api.com", "method": "GET"})).is_ok());
        
        // Happy path (POST)
        assert!(registry.validate(&id, &json!({"url": "http://api.com", "method": "POST", "name": "my_client"})).is_ok());
        
        // Invalid method (lowercase)
        assert!(registry.validate(&id, &json!({"url": "http://api.com", "method": "post"})).is_err());
    }

    #[test]
    fn test_db_writer_validation() {
        let registry = TemplateRegistry::with_builtins();
        let id = TemplateId::new("integration.db_writer").unwrap();
        
        // Happy path
        assert!(registry.validate(&id, &json!({"db_path": "db.sqlite", "query": "INSERT INTO t VALUES (?1)"})).is_ok());
        
        // Missing required field "query"
        assert!(registry.validate(&id, &json!({"db_path": "db.sqlite"})).is_err());
    }

    #[test]
    fn test_http_client_emission_parses_as_rust() {
        let (root, graph, node) = test_ctx("integration.http_client", json!({"url": "https://httpbin.org/post", "method": "POST", "name": "bin_client"}));
        let template = IntegrationHttpClient::new();
        let emission = template.emit_runtime(&CodegenCtx {
            project_slug: "test_proj",
            node: &node,
            output_root: &root,
            graph: &graph,
        }).unwrap();

        assert_eq!(emission.items.len(), 1);
        let item = &emission.items[0];
        assert_eq!(item.module_path, "integrations/bin_client.rs");
        assert!(syn::parse_file(&item.source).is_ok(), "http_client source must be valid Rust");
    }

    #[test]
    fn test_db_writer_emission_parses_as_rust() {
        let (root, graph, node) = test_ctx("integration.db_writer", json!({"db_path": "prod.db", "query": "UPDATE u SET a = ?1", "name": "updater"}));
        let template = IntegrationDbWriter::new();
        let emission = template.emit_runtime(&CodegenCtx {
            project_slug: "test_proj",
            node: &node,
            output_root: &root,
            graph: &graph,
        }).unwrap();

        assert_eq!(emission.items.len(), 1);
        let item = &emission.items[0];
        assert_eq!(item.module_path, "integrations/updater.rs");
        assert!(syn::parse_file(&item.source).is_ok(), "db_writer source must be valid Rust");
    }
}
