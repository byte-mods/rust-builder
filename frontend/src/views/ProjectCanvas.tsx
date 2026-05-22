import { useCallback, useEffect, useRef, useState, useMemo } from "react";
import {
  Background,
  Controls,
  MiniMap,
  ReactFlow,
  ReactFlowProvider,
  addEdge,
  useNodesState,
  useEdgesState,
  useReactFlow,
  type Node,
  type Edge,
  type Connection,
} from "@xyflow/react";
import {
  ApiError,
  EMPTY_GRAPH,
  loadGraph,
  saveGraph,
  buildWebSocketUrl,
  triggerBuild,
  type BuildEvent,
  type Graph,
  type GraphNode,
  type GraphEdge,
  type Template,
} from "../api";
import NodePalette from "./NodePalette";
import NodeConfigDrawer from "./NodeConfigDrawer";
import EntryPointNode from "./EntryPointNode";

interface ProjectCanvasProps {
  slug: string;
  onBack: () => void;
}

type LoadState =
  | { kind: "loading" }
  | { kind: "ready"; graph: Graph }
  | { kind: "error"; message: string };

type BuildState =
  | { kind: "idle" }
  | { kind: "running" }
  | { kind: "done"; code: number }
  | { kind: "error"; message: string };

interface NodeData extends Record<string, unknown> {
  label: string;
  templateId: string;
  config: unknown;
}

let idCounter = 0;
function makeNodeId(): string {
  return `node_${Date.now()}_${idCounter++}`;
}
function makeEdgeId(): string {
  return `edge_${Date.now()}_${idCounter++}`;
}

function toRfNode(gn: GraphNode): Node<NodeData> {
  const templateId = gn.template_id ?? gn.kind ?? "unknown";
  return {
    id: gn.id,
    type: templateId === "core.entry_point" ? "entryPoint" : "default",
    position: { x: gn.position.x, y: gn.position.y },
    data: {
      label: gn.label ?? gn.id,
      templateId,
      config: gn.config ?? {},
    },
  };
}

function fromRfNode(n: Node<NodeData>): GraphNode {
  return {
    id: n.id,
    template_id: n.data.templateId,
    position: { x: n.position.x, y: n.position.y },
    config: n.data.config,
    label: n.data.label,
  };
}

function toRfEdge(ge: GraphEdge): Edge {
  return {
    id: ge.id,
    source: ge.source,
    target: ge.target,
    sourceHandle: ge.source_port,
    targetHandle: ge.target_port,
  };
}

function fromRfEdge(e: Edge): GraphEdge {
  return {
    id: e.id,
    source: e.source,
    target: e.target,
    source_port: e.sourceHandle ?? "default",
    target_port: e.targetHandle ?? "default",
  };
}

export default function ProjectCanvas({ slug, onBack }: ProjectCanvasProps): JSX.Element {
  return (
    <ReactFlowProvider>
      <ProjectCanvasInner slug={slug} onBack={onBack} />
    </ReactFlowProvider>
  );
}

function ProjectCanvasInner({ slug, onBack }: ProjectCanvasProps): JSX.Element {
  const [state, setState] = useState<LoadState>({ kind: "loading" });
  const [templates, setTemplates] = useState<Template[]>([]);

  const [nodes, setNodes, onNodesChange] = useNodesState<Node<NodeData>>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>([]);
  const { screenToFlowPosition } = useReactFlow();

  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);

  const [buildState, setBuildState] = useState<BuildState>({ kind: "idle" });
  const [buildLines, setBuildLines] = useState<string[]>([]);
  const wsRef = useRef<WebSocket | null>(null);
  const linesEndRef = useRef<HTMLDivElement | null>(null);

  const saveTimerRef = useRef<number | null>(null);

  // Load graph on mount.
  useEffect(() => {
    const controller = new AbortController();
    loadGraph(slug, controller.signal)
      .then((graph) => {
        setState({ kind: "ready", graph });
        setNodes(graph.nodes.map(toRfNode));
        setEdges(graph.edges.map(toRfEdge));
      })
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === "AbortError") return;
        const message = err instanceof ApiError ? err.message : "unknown error";
        setState({ kind: "error", message });
      });
    return () => controller.abort();
  }, [slug, setNodes, setEdges]);

  // Load templates for config drawer.
  useEffect(() => {
    import("../api").then(({ fetchTemplates }) => {
      fetchTemplates().then(setTemplates).catch(() => {});
    });
  }, []);

  // Build WebSocket
  useEffect(() => {
    const ws = new WebSocket(buildWebSocketUrl(slug));
    wsRef.current = ws;
    ws.onmessage = (event) => {
      const data: BuildEvent = JSON.parse(event.data);
      switch (data.stream) {
        case "start":
          setBuildState({ kind: "running" });
          setBuildLines((prev) => [...prev, `> ${data.command}`]);
          break;
        case "stdout":
          setBuildLines((prev) => [...prev, data.line ?? ""]);
          break;
        case "stderr":
          setBuildLines((prev) => [...prev, `[stderr] ${data.line ?? ""}`]);
          break;
        case "exit":
          setBuildState(
            data.code === 0
              ? { kind: "done", code: data.code ?? -1 }
              : { kind: "error", message: `exit code ${data.code ?? -1}` }
          );
          setBuildLines((prev) => [...prev, `[exit code ${data.code ?? -1}]`]);
          break;
      }
    };
    ws.onclose = () => {
      wsRef.current = null;
    };
    return () => ws.close();
  }, [slug]);

  useEffect(() => {
    linesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [buildLines]);

  // Debounced save
  const scheduleSave = useCallback(
    (nextNodes: Node<NodeData>[], nextEdges: Edge[]) => {
      if (saveTimerRef.current) {
        window.clearTimeout(saveTimerRef.current);
      }
      saveTimerRef.current = window.setTimeout(() => {
        const graph: Graph = {
          schema_version: 1,
          nodes: nextNodes.map(fromRfNode),
          edges: nextEdges.map(fromRfEdge),
        };
        saveGraph(slug, graph).catch((err: unknown) => {
          if (err instanceof ApiError) {
            console.error("save failed:", err.message);
          }
        });
      }, 800);
    },
    [slug]
  );

  const onConnect = useCallback(
    (connection: Connection) => {
      setEdges((eds) => {
        const next = addEdge({ ...connection, id: makeEdgeId() }, eds);
        scheduleSave(nodes, next);
        return next;
      });
    },
    [setEdges, nodes, scheduleSave]
  );

  const onNodeDoubleClick = useCallback(
    (_event: React.MouseEvent, node: Node<NodeData>) => {
      setSelectedNodeId(node.id);
    },
    []
  );

  const onDragOver = useCallback((event: React.DragEvent) => {
    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
  }, []);

  const onDrop = useCallback(
    (event: React.DragEvent) => {
      event.preventDefault();
      const raw = event.dataTransfer.getData("application/reactflow");
      if (!raw) return;
      const data = JSON.parse(raw) as { templateId: string; defaultConfig: unknown };

      const position = screenToFlowPosition({
        x: event.clientX,
        y: event.clientY,
      });

      const newNode: Node<NodeData> = {
        id: makeNodeId(),
        type: "default",
        position,
        data: {
          label: data.templateId,
          templateId: data.templateId,
          config: data.defaultConfig,
        },
      };

      setNodes((nds) => {
        const next = [...nds, newNode];
        scheduleSave(next, edges);
        return next;
      });
    },
    [screenToFlowPosition, setNodes, edges, scheduleSave]
  );

  const wrappedOnNodesChange = useCallback(
    (changes: Parameters<typeof onNodesChange>[0]) => {
      onNodesChange(changes);
      setNodes((current) => {
        scheduleSave(current, edges);
        return current;
      });
    },
    [onNodesChange, setNodes, edges, scheduleSave]
  );

  const wrappedOnEdgesChange = useCallback(
    (changes: Parameters<typeof onEdgesChange>[0]) => {
      onEdgesChange(changes);
      setEdges((current) => {
        scheduleSave(nodes, current);
        return current;
      });
    },
    [onEdgesChange, setEdges, nodes, scheduleSave]
  );

  const selectedNode = useMemo(
    () => nodes.find((n) => n.id === selectedNodeId) ?? null,
    [nodes, selectedNodeId]
  );

  // Close config drawer if the selected node was deleted.
  useEffect(() => {
    if (selectedNodeId && !nodes.some((n) => n.id === selectedNodeId)) {
      setSelectedNodeId(null);
    }
  }, [nodes, selectedNodeId]);

  function updateNodeConfig(nodeId: string, config: unknown) {
    setNodes((nds) => {
      const next = nds.map((n) =>
        n.id === nodeId ? { ...n, data: { ...n.data, config } } : n
      );
      scheduleSave(next, edges);
      return next;
    });
  }

  async function handleBuild(release = false) {
    setBuildLines([]);
    setBuildState({ kind: "running" });
    try {
      await triggerBuild(slug, release);
    } catch (err: unknown) {
      const message = err instanceof ApiError ? err.message : "failed to start build";
      setBuildState({ kind: "error", message });
      setBuildLines((prev) => [...prev, `[error] ${message}`]);
    }
  }

  const graphToShow = state.kind === "ready" ? state.graph : EMPTY_GRAPH;

  return (
    <div className="project-canvas">
      <header className="project-canvas-header">
        <button type="button" onClick={onBack}>← projects</button>
        <span className="slug-badge"><code>{slug}</code></span>
        <span className="muted">
          {graphToShow.nodes.length} nodes · {graphToShow.edges.length} edges
        </span>
      </header>

      {state.kind === "loading" && <p className="muted center">loading graph…</p>}
      {state.kind === "error" && <p className="form-error center">failed to load: {state.message}</p>}

      {state.kind === "ready" && (
        <div className="project-canvas-body">
          <aside className="project-sidebar">
            <NodePalette />
            <div className="build-panel">
              <h3>Build</h3>
              <div className="build-actions">
                <button
                  type="button"
                  onClick={() => handleBuild(false)}
                  disabled={buildState.kind === "running"}
                >
                  Check
                </button>
                <button
                  type="button"
                  onClick={() => handleBuild(true)}
                  disabled={buildState.kind === "running"}
                >
                  Build Release
                </button>
              </div>
              <BuildBadge state={buildState} />
              <div className="build-output">
                {buildLines.map((line, i) => (
                  <pre key={i}>{line}</pre>
                ))}
                <div ref={linesEndRef} />
              </div>
            </div>
          </aside>

          <main className="studio-canvas">
            <ReactFlow<Node<NodeData>>
              nodes={nodes}
              edges={edges}
              nodeTypes={{ entryPoint: EntryPointNode }}
              onNodesChange={wrappedOnNodesChange}
              onEdgesChange={wrappedOnEdgesChange}
              onConnect={onConnect}
              onNodeDoubleClick={onNodeDoubleClick}
              onDragOver={onDragOver}
              onDrop={onDrop}
              fitView
              deleteKeyCode={["Backspace", "Delete"]}
            >
              <Background />
              <Controls />
              <MiniMap pannable zoomable />
            </ReactFlow>
          </main>

          {selectedNode && (
            <NodeConfigDrawer
              nodeId={selectedNode.id}
              templateId={selectedNode.data.templateId}
              config={selectedNode.data.config}
              templates={templates}
              onChange={(config) => updateNodeConfig(selectedNode.id, config)}
              onClose={() => setSelectedNodeId(null)}
            />
          )}
        </div>
      )}
    </div>
  );
}

function BuildBadge({ state }: { state: BuildState }): JSX.Element {
  switch (state.kind) {
    case "idle":
      return <span className="badge badge-checking">build: idle</span>;
    case "running":
      return <span className="badge badge-checking">build: running…</span>;
    case "done":
      return <span className="badge badge-ok">build: ok</span>;
    case "error":
      return (
        <span className="badge badge-down" title={state.message}>
          build: failed
        </span>
      );
  }
}
