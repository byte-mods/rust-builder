//! Whole-graph dataflow analysis and variable binding mode inference.
//!
//! This module analyzes the visual flow graph to build a value-use graph,
//! tracks task boundaries (`tokio::spawn` and long-running consumers/schedulers),
//! infers optimal Rust variable binding styles (move, reference, Arc, Arc<RwLock>),
//! and performs comment-based placeholder code generation replacements.

use std::collections::{HashMap, HashSet, VecDeque};
use crate::projects::types::{Graph, Node, NodeId};
use crate::templates::TemplateRegistry;

/// Inferred Rust variable binding mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingMode {
    /// Values transferred by value / single-use ownership. Syntax: `var`
    Move,
    /// Values passed by immutable reference / shared read-only borrow. Syntax: `&var`
    Borrow,
    /// Values shared read-only across concurrent task threads. Syntax: `Arc<T>`
    ArcShared,
    /// Values shared read-write/mutably across concurrent task threads. Syntax: `Arc<RwLock<T>>`
    ArcRwLockShared,
}

/// Holds the full dataflow analysis report for one flow graph.
#[derive(Debug)]
pub struct DataflowGraph {
    /// Maps each value (defined by a source node ID and source port) to its inferred binding mode.
    pub bindings: HashMap<(NodeId, String), BindingMode>,
    /// Maps a variable name to its value key so we can easily look it up by variable name.
    pub var_bindings: HashMap<String, BindingMode>,
}

/// Compute a clean, deterministic, valid snake_case Rust identifier for a value definition site.
pub fn get_value_var_name(node_id: &NodeId, port: &str, graph: &Graph) -> String {
    if let Some(node) = graph.nodes.iter().find(|n| n.id == *node_id) {
        if let Some(name_val) = node.config.get("name") {
            if let Some(name_str) = name_val.as_str() {
                let snake = name_str.to_lowercase().replace('.', "_").replace('-', "_");
                return format!("{}_{}", snake, port);
            }
        }
    }
    // Fallback using sanitized node ID digits/letters
    let clean_id = node_id.0.to_lowercase().replace('-', "_").replace('.', "_");
    format!("val_{}_{}", clean_id, port)
}

impl DataflowGraph {
    /// Analyze a visual `graph` and its registered templates to compute binding modes.
    pub fn analyze(graph: &Graph, registry: &TemplateRegistry) -> Self {
        let mut bindings = HashMap::new();
        let mut var_bindings = HashMap::new();

        // 1. Identify all values and their uses.
        // A value is defined by an edge's source (source_node, source_port).
        let mut value_uses: HashMap<(NodeId, String), Vec<(NodeId, String)>> = HashMap::new();
        for edge in &graph.edges {
            value_uses
                .entry((edge.source.clone(), edge.source_port.clone()))
                .or_default()
                .push((edge.target.clone(), edge.target_port.clone()));
        }

        // 2. Identify task execution contexts for each node.
        let mut node_tasks: HashMap<NodeId, HashSet<String>> = HashMap::new();

        // Trace from roots:
        // - http.route node starts an "http" task context.
        // - LongRunner templates start their own task contexts.
        for node in &graph.nodes {
            let is_long_runner = registry
                .get(&node.template_id)
                .map(|t| t.debug_bridge() == crate::templates::DebugBridgeKind::LongRunner)
                .unwrap_or(false);

            if node.template_id.as_str() == "http.route" {
                let ctx_name = "http".to_string();
                Self::mark_reachable(node, &ctx_name, graph, &mut node_tasks);
            } else if is_long_runner || node.template_id.as_str() == "tokio.spawn" || node.template_id.as_str() == "tokio.spawn_blocking" {
                let ctx_name = format!("task_{}", node.id.0);
                Self::mark_reachable(node, &ctx_name, graph, &mut node_tasks);
            }
        }

        // 3. For each value, determine if it is cross-task shared, and if it is mutated.
        for ((src_node, src_port), uses) in &value_uses {
            let mut is_shared = false;
            let mut is_mut = false;

            let src_tasks = node_tasks.get(src_node);

            for (target_node, target_port) in uses {
                // Check mutation in target node config
                if let Some(target_node_obj) = graph.nodes.iter().find(|n| n.id == *target_node) {
                    if target_node_obj.config.get("mutate").and_then(|v| v.as_bool()).unwrap_or(false)
                        || target_node_obj.config.get("mutable").and_then(|v| v.as_bool()).unwrap_or(false)
                        || target_node_obj.config.get("write").and_then(|v| v.as_bool()).unwrap_or(false)
                    {
                        is_mut = true;
                    }
                }
                // Check mutation in target port name
                let tp_lower = target_port.to_lowercase();
                if tp_lower.contains("mut") || tp_lower.contains("write") || tp_lower.contains("update") {
                    is_mut = true;
                }

                // Check cross-task sharing
                let target_tasks = node_tasks.get(target_node);
                match (src_tasks, target_tasks) {
                    (Some(s_tasks), Some(t_tasks)) => {
                        // If their sets of task contexts don't match or have empty overlap:
                        if s_tasks.intersection(t_tasks).count() == 0 {
                            is_shared = true;
                        }
                    }
                    _ => {}
                }
            }

            // Inference logic rules:
            let num_uses = uses.len();
            let mode = if is_shared {
                if is_mut {
                    BindingMode::ArcRwLockShared
                } else {
                    BindingMode::ArcShared
                }
            } else {
                if is_mut {
                    if num_uses > 1 {
                        BindingMode::ArcRwLockShared
                    } else {
                        BindingMode::Move
                    }
                } else {
                    if num_uses > 1 {
                        BindingMode::Borrow
                    } else {
                        BindingMode::Move
                    }
                }
            };

            bindings.insert((src_node.clone(), src_port.clone()), mode);
            
            let var_name = get_value_var_name(src_node, src_port, graph);
            var_bindings.insert(var_name, mode);
        }

        Self { bindings, var_bindings }
    }

    /// Walk downstream from a start node to label all reachable nodes under the same task context.
    fn mark_reachable(
        start_node: &Node,
        ctx_name: &str,
        graph: &Graph,
        node_tasks: &mut HashMap<NodeId, HashSet<String>>,
    ) {
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();

        queue.push_back(start_node.id.clone());
        visited.insert(start_node.id.clone());

        while let Some(current_id) = queue.pop_front() {
            node_tasks
                .entry(current_id.clone())
                .or_default()
                .insert(ctx_name.to_string());

            for edge in &graph.edges {
                if edge.source == current_id && !visited.contains(&edge.target) {
                    visited.insert(edge.target.clone());
                    queue.push_back(edge.target.clone());
                }
            }
        }
    }

    /// Resolve a replacement expression based on the target binding mode.
    pub fn resolve_replacement(&self, op: &str, var_name: &str) -> String {
        let mode = self.var_bindings.get(var_name).cloned().unwrap_or(BindingMode::Move);
        match op {
            "bind" => match mode {
                BindingMode::Move => var_name.to_string(),
                BindingMode::Borrow => format!("&{}", var_name),
                BindingMode::ArcShared => var_name.to_string(),
                BindingMode::ArcRwLockShared => format!("&*({}.read().await)", var_name),
            },
            "bind_mut" => match mode {
                BindingMode::Move => var_name.to_string(),
                BindingMode::Borrow => format!("&mut {}", var_name),
                BindingMode::ArcShared => var_name.to_string(),
                BindingMode::ArcRwLockShared => format!("&mut *({}.write().await)", var_name),
            },
            "clone" => format!("{}.clone()", var_name),
            _ => var_name.to_string(),
        }
    }
}

/// Replace comment placeholders `/*[op:var_name]*/var_name` inside source code using the analyzed dataflow bindings.
pub fn replace_placeholders(source: &str, dataflow: &DataflowGraph) -> String {
    let mut result = String::new();
    let mut current = source;
    
    while let Some(start_idx) = current.find("/*[") {
        result.push_str(&current[..start_idx]);
        let remaining = &current[start_idx..];
        
        if let Some(end_idx) = remaining.find("]*/") {
            let tag = &remaining[3..end_idx]; // e.g. "bind:my_var", "bind_mut:my_var", "clone:my_var"
            let rest = &remaining[end_idx + 3..];
            
            // Skip leading whitespace to get to the identifier
            let ws_len = rest.chars().take_while(|c| c.is_whitespace()).count();
            let id_start = &rest[ws_len..];
            let id_len = id_start.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').count();
            let var_name = &id_start[..id_len];
            
            if let Some((op, target_var)) = tag.split_once(':') {
                if target_var == var_name {
                    let replacement = dataflow.resolve_replacement(op, var_name);
                    result.push_str(&replacement);
                    current = &id_start[id_len..];
                    continue;
                }
            }
            
            result.push_str("/*[");
            current = &remaining[3..];
        } else {
            result.push_str("/*[");
            current = &remaining[3..];
        }
    }
    result.push_str(current);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::types::{Edge, EdgeId, Position};
    use crate::templates::TemplateId;

    fn test_node(id: &str, template_id: &str, config: serde_json::Value) -> Node {
        Node {
            id: NodeId(id.to_string()),
            template_id: TemplateId::new(template_id).unwrap(),
            position: Position { x: 0.0, y: 0.0 },
            config,
            label: None,
            comment: None,
        }
    }

    #[test]
    fn test_move_inference_single_use() {
        let registry = TemplateRegistry::with_builtins();
        let graph = Graph {
            schema_version: 1,
            nodes: vec![
                test_node("n1", "parser.json", serde_json::json!({"name": "parser"})),
                test_node("n2", "core.service", serde_json::json!({"name": "service"})),
            ],
            edges: vec![
                Edge {
                    id: EdgeId("e1".to_string()),
                    source: NodeId("n1".to_string()),
                    target: NodeId("n2".to_string()),
                    source_port: "value".to_string(),
                    target_port: "input".to_string(),
                }
            ],
        };

        let df = DataflowGraph::analyze(&graph, &registry);
        let key = (NodeId("n1".to_string()), "value".to_string());
        assert_eq!(df.bindings.get(&key), Some(&BindingMode::Move));
    }

    #[test]
    fn test_borrow_inference_multi_use_immutable() {
        let registry = TemplateRegistry::with_builtins();
        let graph = Graph {
            schema_version: 1,
            nodes: vec![
                test_node("n1", "parser.json", serde_json::json!({"name": "parser"})),
                test_node("n2", "core.service", serde_json::json!({"name": "svc1"})),
                test_node("n3", "core.service", serde_json::json!({"name": "svc2"})),
            ],
            edges: vec![
                Edge {
                    id: EdgeId("e1".to_string()),
                    source: NodeId("n1".to_string()),
                    target: NodeId("n2".to_string()),
                    source_port: "value".to_string(),
                    target_port: "input".to_string(),
                },
                Edge {
                    id: EdgeId("e2".to_string()),
                    source: NodeId("n1".to_string()),
                    target: NodeId("n3".to_string()),
                    source_port: "value".to_string(),
                    target_port: "input".to_string(),
                }
            ],
        };

        let df = DataflowGraph::analyze(&graph, &registry);
        let key = (NodeId("n1".to_string()), "value".to_string());
        assert_eq!(df.bindings.get(&key), Some(&BindingMode::Borrow));
    }

    #[test]
    fn test_arc_rwlock_inference_multi_use_mutable() {
        let registry = TemplateRegistry::with_builtins();
        let graph = Graph {
            schema_version: 1,
            nodes: vec![
                test_node("n1", "parser.json", serde_json::json!({"name": "parser"})),
                test_node("n2", "core.service", serde_json::json!({"name": "svc1", "mutate": true})),
                test_node("n3", "core.service", serde_json::json!({"name": "svc2"})),
            ],
            edges: vec![
                Edge {
                    id: EdgeId("e1".to_string()),
                    source: NodeId("n1".to_string()),
                    target: NodeId("n2".to_string()),
                    source_port: "value".to_string(),
                    target_port: "input".to_string(),
                },
                Edge {
                    id: EdgeId("e2".to_string()),
                    source: NodeId("n1".to_string()),
                    target: NodeId("n3".to_string()),
                    source_port: "value".to_string(),
                    target_port: "input".to_string(),
                }
            ],
        };

        let df = DataflowGraph::analyze(&graph, &registry);
        let key = (NodeId("n1".to_string()), "value".to_string());
        assert_eq!(df.bindings.get(&key), Some(&BindingMode::ArcRwLockShared));
    }
}
