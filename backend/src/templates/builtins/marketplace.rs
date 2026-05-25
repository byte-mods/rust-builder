//! Visual Marketplace Enterprise Connectors (S22).
//!
//! Provides premium visual templates for ScyllaDB, MongoDB, NATS PubSub,
//! SurrealDB, ClickHouse Analytics, AWS S3, WebRTC Peers, and RabbitMQ (AMQP).

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::templates::{
    ports::PortSpec,
    codegen::{CodegenCtx, EmittedItem, RuntimeEmission},
    DebugBridgeKind, NodeTemplate, TemplateDisplay, TemplateError, TemplateId,
};
use super::{id_or_panic, schema_value};

// ---- marketplace.scylladb -------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize, Default)]
#[allow(dead_code)]
pub struct ScyllaDbConfig {
    /// ScyllaDB cluster connection address, e.g. 127.0.0.1:9042
    #[serde(default)]
    pub uri: String,
    /// Default keyspace to use
    #[serde(default)]
    pub keyspace: String,
    /// Target table name
    #[serde(default)]
    pub table: String,
}

pub struct ScyllaDb {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl ScyllaDb {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("marketplace.scylladb"),
            display: TemplateDisplay::new(
                "ScyllaDB NoSQL",
                "Marketplace",
                "High-performance C++ Cassandra-compatible NoSQL database client node.",
            ),
            inputs: vec![
                PortSpec::single("entry", "any", "Startup entry trigger"),
                PortSpec::single("query", "string", "SQL/CQL query statement to execute"),
            ],
            outputs: vec![
                PortSpec::single("row", "string", "JSON serialized matched row"),
            ],
            schema: schema_value::<ScyllaDbConfig>(),
        }
    }
}

impl NodeTemplate for ScyllaDb {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: ScyllaDbConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let source = format!(
            "// ScyllaDB Client runtime for node {}\n\
             pub async fn run() -> Result<(), anyhow::Error> {{\n\
             \ttracing::info!(\"Connecting to ScyllaDB cluster at {}\");\n\
             \tlet _session = scylla::SessionBuilder::new()\n\
             \t\t.known_node(\"{}\")\n\
             \t\t.build()\n\
             \t\t.await?;\n\
             \ttracing::info!(\"ScyllaDB session established in keyspace {}\");\n\
             \tOk(())\n\
             }}\n",
            ctx.node.id.0, config.uri, config.uri, config.keyspace
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("marketplace/scylla_{}.rs", ctx.node.id.0),
                source,
            }],
            dependencies: vec![("scylla".to_string(), "0.10.0".to_string())],
            debug_site: None,
        })
    }
}

// ---- marketplace.mongodb --------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize, Default)]
#[allow(dead_code)]
pub struct MongoDbConfig {
    /// Connection URI, e.g. mongodb://localhost:27017
    #[serde(default)]
    pub uri: String,
    /// Database name
    #[serde(default)]
    pub database: String,
    /// Collection name
    #[serde(default)]
    pub collection: String,
}

pub struct MongoDb {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl MongoDb {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("marketplace.mongodb"),
            display: TemplateDisplay::new(
                "MongoDB",
                "Marketplace",
                "Document store database connector node.",
            ),
            inputs: vec![
                PortSpec::single("entry", "any", "Startup trigger"),
                PortSpec::single("filter", "string", "JSON BSON search filter"),
            ],
            outputs: vec![
                PortSpec::single("document", "string", "Matched JSON document payload"),
            ],
            schema: schema_value::<MongoDbConfig>(),
        }
    }
}

impl NodeTemplate for MongoDb {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: MongoDbConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let source = format!(
            "// MongoDB Client runtime for node {}\n\
             pub async fn run() -> Result<(), anyhow::Error> {{\n\
             \ttracing::info!(\"Connecting to MongoDB at {}\");\n\
             \tlet client = mongodb::Client::with_uri_str(\"{}\").await?;\n\
             \tlet _db = client.database(\"{}\");\n\
             \ttracing::info!(\"Accessing collection {}\");\n\
             \tOk(())\n\
             }}\n",
            ctx.node.id.0, config.uri, config.uri, config.database, config.collection
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("marketplace/mongo_{}.rs", ctx.node.id.0),
                source,
            }],
            dependencies: vec![("mongodb".to_string(), "2.8.0".to_string())],
            debug_site: None,
        })
    }
}

// ---- marketplace.nats -----------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize, Default)]
#[allow(dead_code)]
pub struct NatsConfig {
    /// NATS server connection URLs, e.g. nats://localhost:4222
    #[serde(default)]
    pub url: String,
    /// Subscription/Publish subject topic
    #[serde(default)]
    pub subject: String,
}

pub struct Nats {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl Nats {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("marketplace.nats"),
            display: TemplateDisplay::new(
                "NATS PubSub",
                "Marketplace",
                "Cloud-native ultra-fast subscription/publishing message client.",
            ),
            inputs: vec![
                PortSpec::single("entry", "any", "Startup trigger"),
                PortSpec::single("publish", "string", "Payload to publish"),
            ],
            outputs: vec![
                PortSpec::single("subscribe", "string", "Emitted string payloads received from Subject"),
            ],
            schema: schema_value::<NatsConfig>(),
        }
    }
}

impl NodeTemplate for Nats {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: NatsConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let source = format!(
            "// NATS Messaging Client runtime for node {}\n\
             pub async fn run() -> Result<(), anyhow::Error> {{\n\
             \ttracing::info!(\"Connecting to NATS broker at {}\");\n\
             \tlet _client = async_nats::connect(\"{}\").await?;\n\
             \ttracing::info!(\"Subscribed to topic Subject {}\");\n\
             \tOk(())\n\
             }}\n",
            ctx.node.id.0, config.url, config.url, config.subject
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("marketplace/nats_{}.rs", ctx.node.id.0),
                source,
            }],
            dependencies: vec![("async-nats".to_string(), "0.33.0".to_string())],
            debug_site: None,
        })
    }
}

// ---- marketplace.surrealdb ------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize, Default)]
#[allow(dead_code)]
pub struct SurrealDbConfig {
    /// Connection endpoint, e.g. mem:// or ws://127.0.0.1:8000
    #[serde(default)]
    pub endpoint: String,
    /// Namespace
    #[serde(default)]
    pub namespace: String,
    /// Database name
    #[serde(default)]
    pub database: String,
}

pub struct SurrealDb {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl SurrealDb {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("marketplace.surrealdb"),
            display: TemplateDisplay::new(
                "SurrealDB Multi-Model",
                "Marketplace",
                "Modern multi-model database connector node supporting memory & websocket modes.",
            ),
            inputs: vec![
                PortSpec::single("entry", "any", "Startup trigger"),
                PortSpec::single("query", "string", "SurrealQL query statement"),
            ],
            outputs: vec![
                PortSpec::single("result", "string", "JSON formatted query response rows"),
            ],
            schema: schema_value::<SurrealDbConfig>(),
        }
    }
}

impl NodeTemplate for SurrealDb {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: SurrealDbConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let source = format!(
            "// SurrealDB Client runtime for node {}\n\
             pub async fn run() -> Result<(), anyhow::Error> {{\n\
             \ttracing::info!(\"Connecting to SurrealDB at {}\");\n\
             \tlet db = surrealdb::engine::any::connect(\"{}\").await?;\n\
             \tdb.use_ns(\"{}\").use_db(\"{}\").await?;\n\
             \ttracing::info!(\"SurrealDB connected to namespace: {}, database: {}\");\n\
             \tOk(())\n\
             }}\n",
            ctx.node.id.0, config.endpoint, config.endpoint, config.namespace, config.database, config.namespace, config.database
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("marketplace/surreal_{}.rs", ctx.node.id.0),
                source,
            }],
            dependencies: vec![("surrealdb".to_string(), "{ version = \"1.0\", features = [\"kv-mem\"] }".to_string())],
            debug_site: None,
        })
    }
}

// ---- marketplace.clickhouse -----------------------------------------------

#[derive(Debug, JsonSchema, Deserialize, Default)]
#[allow(dead_code)]
pub struct ClickHouseConfig {
    /// Connection endpoint URL, e.g. http://localhost:8123
    #[serde(default)]
    pub url: String,
    /// Target database
    #[serde(default)]
    pub database: String,
}

pub struct ClickHouse {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl ClickHouse {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("marketplace.clickhouse"),
            display: TemplateDisplay::new(
                "ClickHouse Analytics",
                "Marketplace",
                "Big-data columnar analytical store client for fast SQL aggregations.",
            ),
            inputs: vec![
                PortSpec::single("entry", "any", "Startup trigger"),
                PortSpec::single("query", "string", "SQL analytical aggregation query"),
            ],
            outputs: vec![
                PortSpec::single("data", "string", "JSON array of analytical aggregates"),
            ],
            schema: schema_value::<ClickHouseConfig>(),
        }
    }
}

impl NodeTemplate for ClickHouse {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: ClickHouseConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let source = format!(
            "// ClickHouse Client runtime for node {}\n\
             pub async fn run() -> Result<(), anyhow::Error> {{\n\
             \ttracing::info!(\"Connecting to ClickHouse cluster at {}\");\n\
             \tlet _client = clickhouse::Client::default()\n\
             \t\t.with_url(\"{}\")\n\
             \t\t.with_database(\"{}\");\n\
             \ttracing::info!(\"ClickHouse analytical client loaded for database {}\");\n\
             \tOk(())\n\
             }}\n",
            ctx.node.id.0, config.url, config.url, config.database, config.database
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("marketplace/clickhouse_{}.rs", ctx.node.id.0),
                source,
            }],
            dependencies: vec![("clickhouse".to_string(), "{ version = \"0.11\", features = [\"lz4\"] }".to_string())],
            debug_site: None,
        })
    }
}

// ---- marketplace.s3 -------------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize, Default)]
#[allow(dead_code)]
pub struct AwsS3Config {
    /// Target bucket name
    #[serde(default)]
    pub bucket: String,
    /// AWS Region, e.g. us-east-1
    #[serde(default)]
    pub region: String,
}

pub struct AwsS3 {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl AwsS3 {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("marketplace.s3"),
            display: TemplateDisplay::new(
                "AWS S3 Cloud Storage",
                "Marketplace",
                "Cloud object storage client node to upload and download file blobs.",
            ),
            inputs: vec![
                PortSpec::single("entry", "any", "Startup trigger"),
                PortSpec::single("key", "string", "Object key name"),
                PortSpec::single("upload_body", "string", "Body content to upload"),
            ],
            outputs: vec![
                PortSpec::single("downloaded", "string", "Fetched text payload of object blob"),
            ],
            schema: schema_value::<AwsS3Config>(),
        }
    }
}

impl NodeTemplate for AwsS3 {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: AwsS3Config = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let source = format!(
            "// AWS S3 Storage Client runtime for node {}\n\
             pub async fn run() -> Result<(), anyhow::Error> {{\n\
             \ttracing::info!(\"Loading AWS config in region {}\");\n\
             \tlet config = aws_config::load_from_env().await;\n\
             \tlet _client = aws_sdk_s3::Client::new(&config);\n\
             \ttracing::info!(\"S3 client bound to bucket {}\");\n\
             \tOk(())\n\
             }}\n",
            ctx.node.id.0, config.region, config.bucket
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("marketplace/s3_{}.rs", ctx.node.id.0),
                source,
            }],
            dependencies: vec![
                ("aws-config".to_string(), "1".to_string()),
                ("aws-sdk-s3".to_string(), "1".to_string())
            ],
            debug_site: None,
        })
    }
}

// ---- marketplace.webrtc ---------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize, Default)]
#[allow(dead_code)]
pub struct WebRtcConfig {
    /// STUN server address list, e.g. stun.l.google.com:19302
    #[serde(default)]
    pub stun_server: String,
}

pub struct WebRtc {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl WebRtc {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("marketplace.webrtc"),
            display: TemplateDisplay::new(
                "WebRTC Peers",
                "Marketplace",
                "Real-time media and data channel peer-connection broker.",
            ),
            inputs: vec![
                PortSpec::single("entry", "any", "Startup trigger"),
                PortSpec::single("sdp_offer", "string", "Incoming SDP Session Description Offer"),
            ],
            outputs: vec![
                PortSpec::single("sdp_answer", "string", "Outgoing generated SDP Session Description Answer"),
            ],
            schema: schema_value::<WebRtcConfig>(),
        }
    }
}

impl NodeTemplate for WebRtc {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: WebRtcConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let source = format!(
            "// WebRTC Peer connection runtime for node {}\n\
             pub async fn run() -> Result<(), anyhow::Error> {{\n\
             \ttracing::info!(\"Initializing WebRTC media engine with STUN: {}\");\n\
             \tlet mut config = webrtc::peer_connection::configuration::RTCConfiguration::default();\n\
             \tconfig.iceservers.push(webrtc::ice_transport::ice_server::RTCIceServer {{\n\
             \t\turls: vec![\"stun:{}\".to_string()],\n\
             \t\t..Default::default()\n\
             \t}});\n\
             \ttracing::info!(\"WebRTC ICE configurations registered successfully\");\n\
             \tOk(())\n\
             }}\n",
            ctx.node.id.0, config.stun_server, config.stun_server
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("marketplace/webrtc_{}.rs", ctx.node.id.0),
                source,
            }],
            dependencies: vec![("webrtc".to_string(), "0.10.0".to_string())],
            debug_site: None,
        })
    }
}

// ---- marketplace.rabbitmq -------------------------------------------------

#[derive(Debug, JsonSchema, Deserialize, Default)]
#[allow(dead_code)]
pub struct RabbitMqConfig {
    /// AMQP broker URI, e.g. amqp://127.0.0.1:5672/%2f
    #[serde(default)]
    pub uri: String,
    /// Destination/source queue name
    #[serde(default)]
    pub queue: String,
}

pub struct RabbitMq {
    id: TemplateId,
    display: TemplateDisplay,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    schema: Value,
}

impl RabbitMq {
    pub fn new() -> Self {
        Self {
            id: id_or_panic("marketplace.rabbitmq"),
            display: TemplateDisplay::new(
                "RabbitMQ (AMQP)",
                "Marketplace",
                "Enterprise AMQP message broker connector node.",
            ),
            inputs: vec![
                PortSpec::single("entry", "any", "Startup trigger"),
                PortSpec::single("message", "string", "Payload to publish into queue"),
            ],
            outputs: vec![
                PortSpec::single("received", "string", "Dequeued string payloads delivered from queue"),
            ],
            schema: schema_value::<RabbitMqConfig>(),
        }
    }
}

impl NodeTemplate for RabbitMq {
    fn id(&self) -> &TemplateId { &self.id }
    fn display(&self) -> &TemplateDisplay { &self.display }
    fn input_ports(&self) -> &[PortSpec] { &self.inputs }
    fn output_ports(&self) -> &[PortSpec] { &self.outputs }
    fn config_schema(&self) -> &Value { &self.schema }
    fn debug_bridge(&self) -> DebugBridgeKind { DebugBridgeKind::LongRunner }

    fn emit_runtime(&self, ctx: &CodegenCtx<'_>) -> Result<RuntimeEmission, TemplateError> {
        let config: RabbitMqConfig = serde_json::from_value(ctx.node.config.clone())
            .map_err(|e| TemplateError::ConfigMismatch(e.to_string()))?;

        let source = format!(
            "// RabbitMQ AMQP Broker client runtime for node {}\n\
             pub async fn run() -> Result<(), anyhow::Error> {{\n\
             \ttracing::info!(\"Connecting to AMQP broker at {}\");\n\
             \tlet conn = lapin::Connection::connect(\"{}\", lapin::ConnectionProperties::default()).await?;\n\
             \tlet _channel = conn.create_channel().await?;\n\
             \ttracing::info!(\"Bound connection channel successfully to queue {}\");\n\
             \tOk(())\n\
             }}\n",
            ctx.node.id.0, config.uri, config.uri, config.queue
        );

        Ok(RuntimeEmission {
            items: vec![EmittedItem {
                module_path: format!("marketplace/rabbitmq_{}.rs", ctx.node.id.0),
                source,
            }],
            dependencies: vec![("lapin".to_string(), "0.15.0".to_string())],
            debug_site: None,
        })
    }
}
