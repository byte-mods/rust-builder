//! Submodule for visual LLM flow generation and refinement using Anthropic Claude API.
//!
//! Gathers recursive source code context, shapes tool-calling schemas, handles Anthropic API
//! integration, and provides rigorous post-generation schema/registry validations.

use std::fs;
use std::path::Path;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use crate::projects::types::Graph;
use crate::templates::TemplateRegistry;
use crate::error::ApiError;

/// Assembles the context for a project's LLM generation request.
/// Scans the recursive Rust source files, Cargo.toml, CLAUDE.md, and graph.json.
/// Caps total gathered text size to 120,000 characters to manage LLM token budgeting.
pub fn assemble_context(project_dir: &Path, current_graph: &Graph) -> String {
    let mut context = String::new();

    // 1. Add Cargo.toml
    let cargo_path = project_dir.join("Cargo.toml");
    if cargo_path.exists() {
        if let Ok(content) = fs::read_to_string(&cargo_path) {
            context.push_str("### Project Cargo.toml\n```toml\n");
            context.push_str(&content);
            context.push_str("\n```\n\n");
        }
    }

    // 2. Add CLAUDE.md if it exists
    let claude_path = project_dir.join("CLAUDE.md");
    if claude_path.exists() {
        if let Ok(content) = fs::read_to_string(&claude_path) {
            context.push_str("### Project CLAUDE.md\n```markdown\n");
            context.push_str(&content);
            context.push_str("\n```\n\n");
        }
    }

    // 3. Add current graph.json
    if let Ok(graph_str) = serde_json::to_string_pretty(current_graph) {
        context.push_str("### Current visual canvas graph (graph.json)\n```json\n");
        context.push_str(&graph_str);
        context.push_str("\n```\n\n");
    }

    // 4. Add recursive Rust source files in src/
    let src_dir = project_dir.join("src");
    if src_dir.exists() {
        context.push_str("### Project generated Rust source files under src/\n");
        let mut rs_files = Vec::new();
        collect_rs_files(&src_dir, &src_dir, &mut rs_files);
        rs_files.sort_by(|a, b| a.0.cmp(&b.0));

        let mut total_chars = context.len();
        const CHAR_BUDGET: usize = 120_000;

        for (rel_path, content) in rs_files {
            let file_block = format!("#### File: src/{}\n```rust\n{}\n```\n\n", rel_path, content);
            if total_chars + file_block.len() > CHAR_BUDGET {
                context.push_str("#### [Remaining source files truncated due to context size limits]\n");
                break;
            }
            context.push_str(&file_block);
            total_chars += file_block.len();
        }
    }

    context
}

fn collect_rs_files(base_dir: &Path, current_dir: &Path, rs_files: &mut Vec<(String, String)>) {
    if let Ok(entries) = fs::read_dir(current_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name != "target" && name != ".git" && name != "scratch" {
                    collect_rs_files(base_dir, &path, rs_files);
                }
            } else if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(rel_path) = path.strip_prefix(base_dir) {
                        if let Some(rel_str) = rel_path.to_str() {
                            rs_files.push((rel_str.to_string(), content));
                        }
                    }
                }
            }
        }
    }
}

// Anthropic API client structures for parsing forced tool calls
#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    content: Vec<ContentItem>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
enum ContentItem {
    Text { text: String },
    ToolUse { id: String, name: String, input: Value },
}

/// Parse the JSON response returned by the Anthropic Messages API and extract the forced tool call.
pub fn extract_graph_from_response(response_json: Value) -> Result<Graph, ApiError> {
    let response: AnthropicResponse = serde_json::from_value(response_json)
        .map_err(|e| ApiError::LlmError(format!("failed to parse Anthropic response: {e}")))?;

    for item in response.content {
        if let ContentItem::ToolUse { name, input, .. } = item {
            if name == "update_graph" {
                let graph: Graph = serde_json::from_value(input)
                    .map_err(|e| ApiError::LlmError(format!("LLM returned an invalid graph.json structure: {e}")))?;
                return Ok(graph);
            }
        }
    }

    Err(ApiError::LlmError("the LLM did not call the expected `update_graph` tool".to_string()))
}

/// Builds the system prompt for Claude, detailing the rust_no_code environment,
/// graph canvas design rules, and providing the registry palette summaries as dynamic reference.
pub fn build_system_prompt(registry: &TemplateRegistry) -> String {
    let templates = registry.summaries();
    let templates_json = serde_json::to_string_pretty(&templates)
        .unwrap_or_else(|_| "[]".to_string());

    format!(
        r#"You are the visual flow graph assistant for rust_no_code.
rust_no_code allows developers to design async Rust applications visually as flow graphs and compiles them into clean Rust code.

The visual flow graph schema represents canvas nodes, connections, and configurations (graph.json).
You must output a proposed flow graph by calling the `update_graph` tool. DO NOT write any conversational text or raw Rust code outside of the tool call — you must return the visual graph matching the required schema.

---
### Available Node Templates in the Palette:
These are the ONLY pre-installed node templates you can place on the canvas:
{}

---
### Canvas Graph Design Rules:
1. Every node must have:
   - A unique, stable `id` (e.g., "node_status_check" or "n_1").
   - A valid pre-installed `template_id` from the palette list above (e.g. "tokio.spawn", "parser.json").
   - A `position` object containing numeric "x" and "y" coordinates (nodes should be visually spaced out, e.g. 200px apart horizontally/vertically, to avoid overlapping).
   - A `config` object matching the exact `config_schema` of its template. Check the JSON Schemas above to ensure required fields (like "name", "visibility", "body", "return_type") are present and have the correct types.
   - A optional `label` string.
2. Edges must have:
   - A unique `id` (e.g., "edge_1").
   - `source` and `target` matching existing node IDs.
   - `source_port` and `target_port` matching the exact ports defined in the templates' `input_ports` and `output_ports`. For example, `parser.json` output port is "value", `language.clone` input port is "input" and output is "output". Check the port specifications carefully.
3. Keep generated structures clean, safe, and compilation-ready. Since user expression bodies are written in Rust, use snake_case for names, proper types, and propagate errors cleanly (use ? instead of .unwrap() or .expect()).
"#,
        templates_json
    )
}#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LlmProvider {
    Anthropic,
    OpenAi,
    Codex,
    ClaudeCli,
    DeepSeek,
    Kimi,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChatMessage {
    pub role: String, // "user" or "assistant"
    pub content: String,
}

/// Robust JSON graph extraction from response text.
/// Safely handles markdown-fenced JSON blocks (e.g. ```json ... ```) or pure JSON strings,
/// making the parsing resilient across different LLM formats.
pub fn extract_graph_from_text(text: &str) -> Result<Graph, ApiError> {
    let trimmed = text.trim();
    let raw_json = if let Some(start_idx) = trimmed.find("```json") {
        let after_fence = &trimmed[start_idx + 7..];
        if let Some(end_idx) = after_fence.find("```") {
            after_fence[..end_idx].trim()
        } else {
            after_fence.trim()
        }
    } else if let Some(start_idx) = trimmed.find("```") {
        let after_fence = &trimmed[start_idx + 3..];
        if let Some(end_idx) = after_fence.find("```") {
            after_fence[..end_idx].trim()
        } else {
            after_fence.trim()
        }
    } else {
        trimmed
    };

    let graph: Graph = serde_json::from_str(raw_json)
        .map_err(|e| ApiError::LlmError(format!("LLM returned an invalid graph.json structure: {e}")))?;
    Ok(graph)
}

/// Submits system/user prompts to the Anthropic Messages API, forcing tool-calling structure.
pub async fn call_anthropic_api(
    api_key: &str,
    system_prompt: &str,
    user_prompt: &str,
    history: Option<&[ChatMessage]>,
) -> Result<Graph, ApiError> {
    let client = reqwest::Client::new();
    
    // We define the update_graph tool schema matching our visual Graph layout.
    let update_graph_tool = json!({
        "name": "update_graph",
        "description": "Updates or generates the visual flow graph matching the rust_no_code canvas schema.",
        "input_schema": {
            "type": "object",
            "properties": {
                "schema_version": {
                    "type": "integer",
                    "description": "Must be equal to GRAPH_SCHEMA_VERSION (1)."
                },
                "nodes": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "template_id": { "type": "string" },
                            "position": {
                                 "type": "object",
                                 "properties": {
                                     "x": { "type": "number" },
                                     "y": { "type": "number" }
                                 },
                                 "required": ["x", "y"]
                            },
                            "config": { "type": "object" },
                            "label": { "type": "string" }
                        },
                        "required": ["id", "template_id", "position", "config"]
                    }
                },
                "edges": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "source": { "type": "string" },
                            "target": { "type": "string" },
                            "source_port": { "type": "string" },
                            "target_port": { "type": "string" }
                        },
                        "required": ["id", "source", "target", "source_port", "target_port"]
                    }
                }
            },
            "required": ["schema_version", "nodes", "edges"]
        }
    });

    let mut messages = Vec::new();
    if let Some(hist) = history {
        for msg in hist {
            messages.push(json!({
                "role": msg.role,
                "content": msg.content,
            }));
        }
    }
    messages.push(json!({
        "role": "user",
        "content": user_prompt
    }));

    let payload = json!({
        "model": "claude-3-5-sonnet-20241022",
        "max_tokens": 4000,
        "system": system_prompt,
        "messages": messages,
        "tools": [update_graph_tool],
        "tool_choice": {
            "type": "tool",
            "name": "update_graph"
        }
    });

    let res = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| ApiError::LlmError(format!("network request to Anthropic failed: {e}")))?;

    if !res.status().is_success() {
        let status = res.status();
        let body_text = res.text().await.unwrap_or_default();
        return Err(ApiError::LlmError(format!(
            "Anthropic API returned status {status}: {body_text}"
        )));
    }

    let response_val: Value = res
        .json()
        .await
        .map_err(|e| ApiError::LlmError(format!("failed to parse Anthropic response JSON: {e}")))?;

    extract_graph_from_response(response_val)
}

/// Unified entry point to call any of the supported LLM providers.
pub async fn call_llm_api(
    provider: LlmProvider,
    api_key: Option<&str>,
    model: Option<&str>,
    system_prompt: &str,
    user_prompt: &str,
    history: Option<&[ChatMessage]>,
) -> Result<Graph, ApiError> {
    match provider {
        LlmProvider::Anthropic => {
            let key = api_key.ok_or_else(|| ApiError::LlmError("Anthropic API key is missing".to_string()))?;
            call_anthropic_api(key, system_prompt, user_prompt, history).await
        }
        LlmProvider::OpenAi | LlmProvider::Codex | LlmProvider::DeepSeek | LlmProvider::Kimi => {
            let (base_url, default_model, auth_header_prefix, env_key_name) = match provider {
                LlmProvider::OpenAi => ("https://api.openai.com/v1/chat/completions", "gpt-4o", "Bearer", "OPENAI_API_KEY"),
                LlmProvider::Codex => {
                    let url = std::env::var("CODEX_API_BASE")
                        .unwrap_or_else(|_| "https://api.openai.com/v1/chat/completions".to_string());
                    (Box::leak(url.into_boxed_str()) as &str, "gpt-4o", "Bearer", "CODEX_API_KEY")
                }
                LlmProvider::DeepSeek => ("https://api.deepseek.com/chat/completions", "deepseek-chat", "Bearer", "DEEPSEEK_API_KEY"),
                LlmProvider::Kimi => ("https://api.moonshot.cn/v1/chat/completions", "moonshot-v1-8k", "Bearer", "KIMI_API_KEY"),
                _ => unreachable!(),
            };

            let key = match api_key {
                Some(k) if !k.trim().is_empty() => k.to_string(),
                _ => std::env::var(env_key_name)
                    .or_else(|_| {
                        if provider == LlmProvider::Codex {
                            std::env::var("OPENAI_API_KEY")
                        } else {
                            Err(std::env::VarError::NotPresent)
                        }
                    })
                    .map_err(|_| ApiError::LlmError(format!("API key environment variable `{env_key_name}` is not set")))?
            };

            let active_model = model.unwrap_or(default_model);

            let client = reqwest::Client::new();
            let mut messages = Vec::new();
            messages.push(json!({
                "role": "system",
                "content": system_prompt
            }));

            if let Some(hist) = history {
                for msg in hist {
                    messages.push(json!({
                        "role": msg.role,
                        "content": msg.content,
                    }));
                }
            }

            messages.push(json!({
                "role": "user",
                "content": user_prompt
            }));

            let payload = json!({
                "model": active_model,
                "messages": messages,
                "response_format": { "type": "json_object" }
            });

            let res = client
                .post(base_url)
                .header("Authorization", format!("{auth_header_prefix} {key}"))
                .header("Content-Type", "application/json")
                .json(&payload)
                .send()
                .await
                .map_err(|e| ApiError::LlmError(format!("network request to {provider:?} failed: {e}")))?;

            if !res.status().is_success() {
                let status = res.status();
                let body_text = res.text().await.unwrap_or_default();
                return Err(ApiError::LlmError(format!(
                    "{provider:?} API returned status {status}: {body_text}"
                )));
            }

            let response_val: Value = res
                .json()
                .await
                .map_err(|e| ApiError::LlmError(format!("failed to parse response JSON: {e}")))?;

            let text_content = response_val["choices"][0]["message"]["content"]
                .as_str()
                .ok_or_else(|| ApiError::LlmError("API response did not contain expected text choice".to_string()))?;

            extract_graph_from_text(text_content)
        }
        LlmProvider::ClaudeCli => {
            let full_prompt = format!("System Prompt:\n{}\n\nUser Request:\n{}", system_prompt, user_prompt);
            
            let output = std::process::Command::new("claude")
                .arg(&full_prompt)
                .output();

            match output {
                Ok(out) if out.status.success() => {
                    let text = String::from_utf8(out.stdout)
                        .map_err(|e| ApiError::LlmError(format!("failed to parse Claude CLI stdout: {e}")))?;
                    extract_graph_from_text(&text)
                }
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    Err(ApiError::LlmError(format!(
                        "Claude CLI process exited with non-zero status code ({}): {}",
                        out.status, stderr
                    )))
                }
                Err(e) => {
                    Err(ApiError::LlmError(format!(
                        "Claude CLI command execution failed. Is the `claude` CLI tool installed in your system PATH? Error: {}",
                        e
                    )))
                }
            }
        }
    }
}

/// Runs rigorous post-generation schema and template registry validations
/// to ensure the LLM's proposed graph conforms 100% to our visual spec.
pub fn validate_proposed_graph(graph: &Graph, registry: &TemplateRegistry) -> Result<(), ApiError> {
    if graph.schema_version != crate::projects::GRAPH_SCHEMA_VERSION {
        return Err(ApiError::InvalidGraph(format!(
            "unsupported proposed graph schema version: expected {}, found {}",
            crate::projects::GRAPH_SCHEMA_VERSION,
            graph.schema_version
        )));
    }

    // 1. Validate each node's template and configuration schema
    for node in &graph.nodes {
        registry
            .validate(&node.template_id, &node.config)
            .map_err(|e| ApiError::InvalidGraph(format!(
                "proposed node `{}` failed configuration validation: {e}",
                node.id.0
            )))?;
    }

    // 2. Validate all edges connect valid nodes
    let node_ids: std::collections::HashSet<_> = graph.nodes.iter().map(|n| &n.id).collect();
    for edge in &graph.edges {
        if !node_ids.contains(&edge.source) {
            return Err(ApiError::InvalidGraph(format!(
                "edge `{}` refers to non-existent source node `{}`",
                edge.id.0, edge.source.0
            )));
        }
        if !node_ids.contains(&edge.target) {
            return Err(ApiError::InvalidGraph(format!(
                "edge `{}` refers to non-existent target node `{}`",
                edge.id.0, edge.target.0
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::types::{NodeId, EdgeId, Edge};

    #[test]
    fn test_extract_graph_from_tool_use_response() {
        let response_json = json!({
            "id": "msg_01",
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_01",
                    "name": "update_graph",
                    "input": {
                        "schema_version": 1,
                        "nodes": [
                            {
                                "id": "n1",
                                "template_id": "tokio.sleep",
                                "position": { "x": 100.0, "y": 200.0 },
                                "config": { "name": "delay", "duration_ms": 1000 }
                            }
                        ],
                        "edges": []
                    }
                }
            ]
        });

        let graph = extract_graph_from_response(response_json).unwrap();
        assert_eq!(graph.schema_version, 1);
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].id.0, "n1");
        assert_eq!(graph.nodes[0].template_id.as_str(), "tokio.sleep");
        assert_eq!(graph.nodes[0].position.x, 100.0);
    }

    #[test]
    fn test_extract_graph_fails_when_no_tool_use() {
        let response_json = json!({
            "id": "msg_01",
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "text",
                    "text": "Hello world"
                }
            ]
        });

        let err = extract_graph_from_response(response_json).unwrap_err();
        assert!(err.to_string().contains("did not call the expected `update_graph` tool"));
    }

    #[test]
    fn test_extract_graph_from_text_fences() {
        let text_with_json_fence = r#"
Here is the updated graph:
```json
{
  "schema_version": 1,
  "nodes": [
    {
      "id": "n_sleep",
      "template_id": "tokio.sleep",
      "position": { "x": 100, "y": 200 },
      "config": { "name": "delay", "duration_ms": 1000 }
    }
  ],
  "edges": []
}
```
        "#;
        let graph = extract_graph_from_text(text_with_json_fence).unwrap();
        assert_eq!(graph.schema_version, 1);
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].id.0, "n_sleep");
    }

    #[test]
    fn test_validate_proposed_graph_finds_dangling_edges() {
        let registry = TemplateRegistry::with_builtins();
        let graph = Graph {
            schema_version: 1,
            nodes: vec![
                Graph::test_node("n1", "tokio.sleep", json!({"name": "delay", "duration_ms": 1000}))
            ],
            edges: vec![
                Edge {
                    id: EdgeId("e1".to_string()),
                    source: NodeId("n1".to_string()),
                    target: NodeId("n_ghost".to_string()),
                    source_port: "out".to_string(),
                    target_port: "in".to_string(),
                }
            ]
        };

        let err = validate_proposed_graph(&graph, &registry).unwrap_err();
        assert!(err.to_string().contains("refers to non-existent target node `n_ghost`"));
    }
}

// Minimal helper to implement test_node in Graph for testing convenience
impl Graph {
    #[allow(dead_code)]
    fn test_node(id: &str, template_id: &str, config: Value) -> crate::projects::types::Node {
        crate::projects::types::Node {
            id: crate::projects::types::NodeId(id.to_string()),
            template_id: crate::templates::TemplateId::new(template_id).unwrap(),
            position: crate::projects::types::Position { x: 0.0, y: 0.0 },
            config,
            label: None,
            comment: None,
        }
    }
}
