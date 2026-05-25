//! Universal Connectors Pack templates (S16/S17).
//!
//! Provides templates for Kafka Consumer, Kafka Producer, Redis Cache,
//! and PostgreSQL/SQL Connector.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::templates::{
    ports::PortSpec,
    codegen::{CodegenCtx, EmittedItem, RuntimeEmission},
    DebugBridgeKind, NodeTemplate, TemplateDisplay, TemplateError, TemplateId,
};
use super::{id_or_panic, schema_value, to_snake_case};

// ---- integration.kafka_consumer -------------------------------------------

#[derive(Debug, JsonSchema, Deserialize, Default)]
#[allow(dead_code)]
pub struct KafkaConsumerConfig {
    /// Kafka brokers comma-separated list, e.g. localhost:9092.
    #[serde(default)]
    pub brokers: String,
    /// Topic name to poll from.
    #[serde(default)]
    pub topic: String,
    /// Consumer group identifier.
    #[serde(default)]
    pub group: String,
    /// Snake_case module name. Defaults to topic name with dashes replaced by underscores.
    #[serde(default)]
    pub name: Option<String>,
}

pub struct IntegrationKafkaConsumer {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl IntegrationKafkaConsumer {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("integration.kafka_consumer"),
            display: TemplateDisplay::new(
                "Kafka Consumer",
                "Integration",
                "Long-running consumer that polls message payloads from a Kafka broker/topic.",
            ),
            inputs: vec![PortSpec::single(
                "entry",
                "any",
                "Application entry — wire from core.entry_point to spawn this consumer at startup.",
            )],
            outputs: vec![PortSpec::single(
                "message",
                "string",
                "Emitted raw message payload string per delivery.",
            )],
            schema: schema_value::<KafkaConsumerConfig>(),
        }
    }
}

impl NodeTemplate for IntegrationKafkaConsumer {
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
        let config: KafkaConsumerConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let name = config.name.unwrap_or_else(|| {
            if config.topic.is_empty() {
                "kafka_consumer".to_string()
            } else {
                config.topic.replace('-', "_")
            }
        });
        let brokers = config.brokers;
        let topic = config.topic;
        let group = config.group;

        let mut downstream_uses = String::new();
        let mut downstream_calls = String::new();

        for edge in &ctx.graph.edges {
            if edge.source == ctx.node.id && edge.source_port == "message" {
                if let Some(target) = ctx.graph.nodes.iter().find(|n| n.id == edge.target) {
                    if target.template_id.as_str() == "core.service" {
                        let svc_name = target.config.get("name").and_then(|v| v.as_str()).unwrap();
                        downstream_uses.push_str(&format!("use crate::services::{};\n", svc_name));
                        downstream_calls.push_str(&format!("        let _ = {}::{}(msg.clone()).await;\n", svc_name, svc_name));
                    } else if target.template_id.as_str() == "integration.db_writer" {
                        let db_name = target.config.get("name").and_then(|v| v.as_str()).unwrap();
                        downstream_uses.push_str(&format!("use crate::integrations::{};\n", db_name));
                        downstream_calls.push_str(&format!("        let _ = {}::execute(vec![msg.clone()]).await;\n", db_name));
                    } else if target.template_id.as_str() == "custom.block" {
                        let custom_name = target.config.get("name").and_then(|v| v.as_str()).unwrap();
                        downstream_uses.push_str(&format!("use crate::functions::{};\n", custom_name));
                        downstream_calls.push_str(&format!("        let _ = {}::{}(msg.clone()).await;\n", custom_name, custom_name));
                    }
                }
            }
        }

        let source = format!(
            r#"use crate::errors::AppError;
use tracing::{{info, error}};
use rdkafka::consumer::{{Consumer, StreamConsumer}};
use rdkafka::ClientConfig;
use rdkafka::Message;
{downstream_uses}

pub async fn run() {{
    let brokers = "{brokers}";
    let topic = "{topic}";
    let group = "{group}";
    info!(%brokers, %topic, %group, "connecting to Kafka consumer...");

    let consumer_res: Result<StreamConsumer, _> = ClientConfig::new()
        .set("group.id", group)
        .set("bootstrap.servers", brokers)
        .set("enable.partition.eof", "false")
        .set("session.timeout.ms", "6000")
        .set("enable.auto.commit", "true")
        .create();

    let consumer = match consumer_res {{
        Ok(c) => c,
        Err(e) => {{
            error!("Consumer creation failed: {{}}", e);
            return;
        }}
    }};

    if let Err(e) = consumer.subscribe(&[topic]) {{
        error!("Failed to subscribe to topic: {{}}", e);
        return;
    }}

    loop {{
        match consumer.recv().await {{
            Err(e) => error!("Kafka error: {{}}", e),
            Ok(m) => {{
                if let Some(payload) = m.payload_view::<str>() {{
                    match payload {{
                        Ok(msg_str) => {{
                            let msg = msg_str.to_string();
                            info!(%topic, "polled message from Kafka");
{downstream_calls}
                        }}
                        Err(e) => error!("Error deserializing message payload: {{}}", e),
                    }}
                }}
            }}
        }}
    }}
}}
"#,
            brokers = brokers,
            topic = topic,
            group = group,
            downstream_uses = downstream_uses,
            downstream_calls = downstream_calls
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("consumers/{}.rs", to_snake_case(&name)),
                source,
            }],
            dependencies: vec![
                ("rdkafka".to_string(), "\"0.36\"".to_string()),
            ],
            debug_site: None,
        })
    }
}

// ---- integration.kafka_producer -------------------------------------------

#[derive(Debug, JsonSchema, Deserialize, Default)]
#[allow(dead_code)]
pub struct KafkaProducerConfig {
    /// Kafka brokers comma-separated list.
    #[serde(default)]
    pub brokers: String,
    /// Topic name to publish messages to.
    #[serde(default)]
    pub topic: String,
    /// Snake_case module name. Defaults to "kafka_producer".
    #[serde(default)]
    pub name: Option<String>,
}

pub struct IntegrationKafkaProducer {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl IntegrationKafkaProducer {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("integration.kafka_producer"),
            display: TemplateDisplay::new(
                "Kafka Producer",
                "Integration",
                "Publishes message payloads to a Kafka topic.",
            ),
            inputs: vec![PortSpec::single(
                "payload",
                "string",
                "Message payload string to publish.",
            )],
            outputs: vec![PortSpec::single(
                "status",
                "string",
                "Acknowledgment status of the publication.",
            )],
            schema: schema_value::<KafkaProducerConfig>(),
        }
    }
}

impl NodeTemplate for IntegrationKafkaProducer {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }

    fn emit_runtime(
        &self,
        ctx: &CodegenCtx<'_>,
    ) -> Result<RuntimeEmission, TemplateError> {
        let config: KafkaProducerConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let name = config.name.unwrap_or_else(|| "kafka_producer".to_string());
        let brokers = config.brokers;
        let topic = config.topic;

        let source = format!(
            r#"use crate::errors::AppError;
use tracing::{{info, error}};
use rdkafka::producer::{{FutureProducer, FutureRecord}};
use rdkafka::ClientConfig;
use std::time::Duration;

pub async fn send_message(payload: String) -> Result<String, AppError> {{
    let brokers = "{brokers}";
    let topic = "{topic}";
    info!(%brokers, %topic, "publishing message to Kafka");

    let producer: FutureProducer = ClientConfig::new()
        .set("bootstrap.servers", brokers)
        .set("message.timeout.ms", "5000")
        .create()
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    let record = FutureRecord::to(topic).payload(&payload).key("");
    
    match producer.send(record, Duration::from_secs(0)).await {{
        Ok((partition, offset)) => Ok(format!("Ack: Sent to partition {{}} at offset {{}}", partition, offset)),
        Err((e, _)) => Err(AppError::BadRequest(e.to_string())),
    }}
}}
"#,
            brokers = brokers,
            topic = topic
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("integrations/{}.rs", to_snake_case(&name)),
                source,
            }],
            dependencies: vec![
                ("rdkafka".to_string(), "\"0.36\"".to_string()),
            ],
            debug_site: None,
        })
    }
}

// ---- integration.redis ----------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize, Default)]
#[allow(dead_code)]
pub struct RedisConfig {
    /// Redis connection string, e.g. redis://127.0.0.1:6379.
    #[serde(default)]
    pub connection_string: String,
    /// Operation to perform (GET or SET).
    #[serde(default)]
    pub operation: String,
    /// Snake_case module name. Defaults to "redis_client".
    #[serde(default)]
    pub name: Option<String>,
}

pub struct IntegrationRedis {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl IntegrationRedis {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("integration.redis"),
            display: TemplateDisplay::new(
                "Redis Cache",
                "Integration",
                "Executes key/value cache operations on a Redis instance.",
            ),
            inputs: vec![
                PortSpec::single("key", "string", "Cache key identifier."),
                PortSpec::single("value", "string", "Cache value to store (ignored for GET)."),
            ],
            outputs: vec![PortSpec::single(
                "result",
                "string",
                "Retrieved value for GET, or status message for SET.",
            )],
            schema: schema_value::<RedisConfig>(),
        }
    }
}

impl NodeTemplate for IntegrationRedis {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }

    fn emit_runtime(
        &self,
        ctx: &CodegenCtx<'_>,
    ) -> Result<RuntimeEmission, TemplateError> {
        let config: RedisConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let name = config.name.unwrap_or_else(|| "redis_client".to_string());
        let connection_string = config.connection_string;
        let operation = config.operation;

        let source = format!(
            r#"use crate::errors::AppError;
use tracing::{{info, error}};
use redis::AsyncCommands;

pub async fn execute(key: String, value: String) -> Result<String, AppError> {{
    let connection_string = "{connection_string}";
    let op = "{operation}";
    info!(%connection_string, op, %key, "connecting and executing Redis command");

    let client = redis::Client::open(connection_string).map_err(|e| AppError::BadRequest(e.to_string()))?;
    let mut con = client.get_multiplexed_tokio_connection().await.map_err(|e| AppError::BadRequest(e.to_string()))?;

    if op == "SET" {{
        info!(%key, "storing value in Redis cache");
        let _: () = con.set(&key, value).await.map_err(|e| AppError::BadRequest(e.to_string()))?;
        Ok(format!("OK - stored '{{}}'", key))
    }} else {{
        info!(%key, "retrieving value from Redis cache");
        let result: String = con.get(&key).await.map_err(|e| AppError::BadRequest(e.to_string()))?;
        Ok(result)
    }}
}}
"#,
            connection_string = connection_string,
            operation = operation
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("integrations/{}.rs", to_snake_case(&name)),
                source,
            }],
            dependencies: vec![
                ("redis".to_string(), "{ version = \"0.25\", features = [\"tokio-comp\"] }".to_string()),
            ],
            debug_site: None,
        })
    }
}

// ---- integration.sql_connector --------------------------------------------

#[derive(Debug, JsonSchema, Deserialize, Default)]
#[allow(dead_code)]
pub struct SqlConnectorConfig {
    /// PostgreSQL/SQL connection string.
    #[serde(default)]
    pub connection_string: String,
    /// SQL statement with parameter placeholders.
    #[serde(default)]
    pub query: String,
    /// Snake_case module name. Defaults to "sql_connector".
    #[serde(default)]
    pub name: Option<String>,
}

pub struct IntegrationSqlConnector {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl IntegrationSqlConnector {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("integration.sql_connector"),
            display: TemplateDisplay::new(
                "PostgreSQL Connector",
                "Integration",
                "Executes parameterized SQL queries against a PostgreSQL database.",
            ),
            inputs: vec![PortSpec::single(
                "params",
                "any",
                "Parameter values list to bind to query placeholders.",
            )],
            outputs: vec![PortSpec::single(
                "result",
                "string",
                "Query results formatted as JSON array string.",
            )],
            schema: schema_value::<SqlConnectorConfig>(),
        }
    }
}

impl NodeTemplate for IntegrationSqlConnector {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }

    fn emit_runtime(
        &self,
        ctx: &CodegenCtx<'_>,
    ) -> Result<RuntimeEmission, TemplateError> {
        let config: SqlConnectorConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let name = config.name.unwrap_or_else(|| "sql_connector".to_string());
        let connection_string = config.connection_string;
        let query = config.query;

        let source = format!(
            r#"use crate::errors::AppError;
use tracing::{{info, error}};
use sqlx::postgres::PgPoolOptions;

pub async fn execute(params: Vec<String>) -> Result<String, AppError> {{
    let connection_string = "{connection_string}";
    let query_str = "{query}";
    info!(%connection_string, query=query_str, ?params, "executing SQL statement");

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(connection_string).await
        .map_err(|e| AppError::BadRequest(e.to_string()))?;

    // Since we are generating generic SQL execution, we use sqlx::query
    let mut q = sqlx::query(query_str);
    for param in &params {{
        q = q.bind(param);
    }}
    
    // As a generic connector, we just execute it and return affected rows.
    let result = q.execute(&pool).await.map_err(|e| AppError::BadRequest(e.to_string()))?;
    
    Ok(format!("[{{{{\"status\": \"success\", \"rows_affected\": {{}}, \"params_used\": {{:?}}}}}}]", result.rows_affected(), params))
}}
"#,
            connection_string = connection_string,
            query = query
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("integrations/{}.rs", to_snake_case(&name)),
                source,
            }],
            dependencies: vec![
                ("sqlx".to_string(), "{ version = \"0.7\", features = [\"postgres\", \"runtime-tokio-rustls\", \"json\"] }".to_string()),
            ],
            debug_site: None,
        })
    }
}
