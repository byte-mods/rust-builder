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
  useViewport,
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
  runWebSocketUrl,
  triggerRun,
  stopRun,
  triggerTest,
  triggerDebug,
  fetchRunStatus,
  type RunEvent,
  generateFlow,
  refineFlow,
  type ChatMessage,
  type ParsedDiagnostic,
  runSecurityAudit,
  type SecurityAuditReport,
} from "../api";
import NodePalette from "./NodePalette";
import NodeConfigDrawer from "./NodeConfigDrawer";
import EntryPointNode from "./EntryPointNode";
import StudioNode from "./StudioNode";
import { SecurityDrawer } from "./SecurityDrawer";


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

function toRfNode(
  gn: GraphNode,
  templates: Template[] = [],
  diagnostics: ParsedDiagnostic[] = []
): Node<NodeData> {
  const templateId = gn.template_id ?? gn.kind ?? "unknown";
  const template = templates.find((t) => t.id === templateId);
  const nodeDiagnostics = diagnostics.filter((d) => d.node_id === gn.id);

  return {
    id: gn.id,
    type: templateId === "core.entry_point" ? "entryPoint" : "studio",
    position: { x: gn.position.x, y: gn.position.y },
    data: {
      label: gn.label ?? gn.id,
      templateId,
      config: gn.config ?? {},
      inputs: (templateId === "custom.block" || templateId === "grpc.server")
        ? (gn.config?.inputs || []).map((p: any) => ({
            name: p.name,
            type_tag: p.ty,
            multiplicity: "single",
            doc: `Custom parameter ${p.name} of type ${p.ty}`,
          }))
        : template?.input_ports ?? [],
      outputs: (templateId === "custom.block" || templateId === "grpc.server")
        ? (gn.config?.outputs || []).map((p: any) => ({
            name: p.name,
            type_tag: p.ty,
            multiplicity: "single",
            doc: `Custom return output of type ${p.ty}`,
          }))
        : template?.output_ports ?? [],
      diagnostics: nodeDiagnostics,
      comment: gn.comment,
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
    comment: n.data.comment,
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

interface Collaborator {
  id: string;
  username: string;
  color: string;
}

function CursorsOverlay({ cursors, collaborators }: { cursors: Record<string, { userId: string, x: number, y: number }>, collaborators: Collaborator[] }): JSX.Element {
  const { x, y, zoom } = useViewport();
  
  return (
    <div style={{ position: "absolute", inset: 0, pointerEvents: "none", zIndex: 10 }}>
      {Object.values(cursors).map((cursor) => {
        const collab = collaborators.find((c) => c.id === cursor.userId);
        if (!collab) return null;
        
        const posX = cursor.x * zoom + x;
        const posY = cursor.y * zoom + y;
        
        return (
          <div
            key={cursor.userId}
            style={{
              position: "absolute",
              left: `${posX}px`,
              top: `${posY}px`,
              transform: "translate(-2px, -2px)",
              transition: "left 0.08s ease-out, top 0.08s ease-out",
              display: "flex",
              flexDirection: "column",
              alignItems: "flex-start",
              pointerEvents: "none",
            }}
          >
            <svg
              width="24"
              height="24"
              viewBox="0 0 24 24"
              fill="none"
              style={{
                filter: "drop-shadow(0 2px 4px rgba(0,0,0,0.4))",
              }}
            >
              <path
                d="M3 3V21L9 15L15 21L17 19L11 13L17 7L3 3Z"
                fill={collab.color}
                stroke="#ffffff"
                strokeWidth="1.5"
                strokeLinejoin="round"
              />
            </svg>
            <div
              style={{
                backgroundColor: collab.color,
                color: "#0f172a",
                padding: "2px 6px",
                borderRadius: "4px",
                fontSize: "10px",
                fontWeight: "bold",
                marginTop: "2px",
                whiteSpace: "nowrap",
                border: "1px solid rgba(255,255,255,0.3)",
                boxShadow: "0 4px 12px rgba(0,0,0,0.3)",
              }}
            >
              {collab.username}
            </div>
          </div>
        );
      })}
    </div>
  );
}

function ProjectCanvasInner({ slug, onBack }: ProjectCanvasProps): JSX.Element {
  const [state, setState] = useState<LoadState>({ kind: "loading" });
  const [templates, setTemplates] = useState<Template[]>([]);
  const [diagnostics, setDiagnostics] = useState<ParsedDiagnostic[]>([]);

  const [nodes, setNodes, onNodesChange] = useNodesState<Node<NodeData>>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>([]);
  const { screenToFlowPosition, setCenter } = useReactFlow();

  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);

  const [buildState, setBuildState] = useState<BuildState>({ kind: "idle" });
  const [buildLines, setBuildLines] = useState<string[]>([]);
  const [isRunning, setIsRunning] = useState<boolean>(false);
  const wsRef = useRef<WebSocket | null>(null);
  const runWsRef = useRef<WebSocket | null>(null);
  const linesEndRef = useRef<HTMLDivElement | null>(null);

  const saveTimerRef = useRef<number | null>(null);

  const myCollabUser = useMemo(() => {
    const key = `collab_user_${slug}`;
    const cached = sessionStorage.getItem(key);
    if (cached) return JSON.parse(cached);
    
    const id = Math.random().toString(36).substring(2, 11);
    const colors = ["#3b82f6", "#ec4899", "#8b5cf6", "#10b981", "#f59e0b", "#ef4444", "#06b6d4"];
    const color = colors[Math.floor(Math.random() * colors.length)];
    const personas = ["Tonic-Scaffolder", "Tokio-Scheduler", "Cargo-Linker", "Rust-Architect", "Wasm-Compiler", "Flow-Designer"];
    const username = `${personas[Math.floor(Math.random() * personas.length)]}-${Math.floor(100 + Math.random() * 900)}`;
    
    const user = { id, username, color };
    sessionStorage.setItem(key, JSON.stringify(user));
    return user;
  }, [slug]);

  const [collaborators, setCollaborators] = useState<Collaborator[]>([]);
  const [cursors, setCursors] = useState<Record<string, { userId: string, x: number, y: number }>>({});
  const collabWsRef = useRef<WebSocket | null>(null);
  const isRemoteUpdateRef = useRef<boolean>(false);
  const lastCursorSendRef = useRef<number>(0);

  // Premium AI Chat Console state variables
  interface SavedChatMessage {
    id: string;
    role: "user" | "assistant";
    content: string;
    timestamp: number;
  }
  interface ChatSession {
    id: string;
    title: string;
    messages: SavedChatMessage[];
    createdAt: number;
  }

  const [sessions, setSessions] = useState<ChatSession[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [isRenamingSession, setIsRenamingSession] = useState(false);
  const [renameTitle, setRenameTitle] = useState("");

  const [aiPrompt, setAiPrompt] = useState("");
  const [aiLoading, setAiLoading] = useState(false);
  const [aiError, setAiError] = useState<string | null>(null);
  const [proposedGraph, setProposedGraph] = useState<Graph | null>(null);

  const activeSession = useMemo(
    () => sessions.find((s) => s.id === activeSessionId) ?? null,
    [sessions, activeSessionId]
  );

  const chatMessagesEndRef = useRef<HTMLDivElement | null>(null);

  // Auto-scroll chat to bottom
  useEffect(() => {
    chatMessagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [activeSession?.messages, aiLoading]);

  // Load chat sessions from localStorage
  useEffect(() => {
    const key = `rust_builder_chats:${slug}`;
    const stored = localStorage.getItem(key);
    if (stored) {
      try {
        const parsed = JSON.parse(stored) as ChatSession[];
        if (parsed.length > 0) {
          setSessions(parsed);
          setActiveSessionId(parsed[0].id);
          return;
        }
      } catch (e) {
        console.error("Failed to parse stored chat sessions:", e);
      }
    }
    // If no sessions, create a default one
    const defaultSession: ChatSession = {
      id: "session_default",
      title: "Design Session 1",
      messages: [],
      createdAt: Date.now(),
    };
    setSessions([defaultSession]);
    setActiveSessionId(defaultSession.id);
  }, [slug]);

  // Save chat sessions to localStorage whenever they change
  useEffect(() => {
    if (sessions.length > 0) {
      const key = `rust_builder_chats:${slug}`;
      localStorage.setItem(key, JSON.stringify(sessions));
    }
  }, [sessions, slug]);

  function handleNewSession() {
    const newSession: ChatSession = {
      id: `session_${Date.now()}`,
      title: `Design Session ${sessions.length + 1}`,
      messages: [],
      createdAt: Date.now(),
    };
    setSessions((prev) => [...prev, newSession]);
    setActiveSessionId(newSession.id);
    setIsRenamingSession(false);
  }

  function handleDeleteSession(sessionId: string) {
    if (sessions.length <= 1) {
      // Don't delete the last remaining session, just clear its messages
      setSessions((prev) =>
        prev.map((s) => (s.id === sessionId ? { ...s, messages: [] } : s))
      );
      return;
    }
    const filtered = sessions.filter((s) => s.id !== sessionId);
    setSessions(filtered);
    if (activeSessionId === sessionId) {
      setActiveSessionId(filtered[0].id);
    }
    setIsRenamingSession(false);
  }

  function handleRenameSession() {
    if (!renameTitle.trim() || !activeSessionId) return;
    setSessions((prev) =>
      prev.map((s) => (s.id === activeSessionId ? { ...s, title: renameTitle.trim() } : s))
    );
    setIsRenamingSession(false);
  }

  const graphToShow = state.kind === "ready" ? state.graph : EMPTY_GRAPH;

  // S13 Step Debugger state variables
  const [activeBreakpoints, setActiveBreakpoints] = useState<string[]>([]);
  const [debugPausedNodeId, setDebugPausedNodeId] = useState<string | null>(null);
  const [inspectedEdgeValues, setInspectedEdgeValues] = useState<Record<string, string>>({});
  const [debugModeActive, setDebugModeActive] = useState<boolean>(false);

  // S21 Performance Profiler state
  const [nodeMetrics, setNodeMetrics] = useState<Record<string, PerformanceStats>>({});

  // S22 Security Audit state
  const [isSecurityOpen, setIsSecurityOpen] = useState<boolean>(false);
  const [securityReport, setSecurityReport] = useState<SecurityAuditReport | null>(null);
  const [securityLoading, setSecurityLoading] = useState<boolean>(false);

  const handleNodeFocus = useCallback((nodeId: string) => {
    setSelectedNodeId(nodeId);
    const node = nodes.find((n) => n.id === nodeId);
    if (node) {
      setCenter(node.position.x + 75, node.position.y + 40, { zoom: 1.25, duration: 800 });
    }
  }, [nodes, setCenter]);

  async function handleSecurityAudit() {
    setIsSecurityOpen(true);
    setSecurityLoading(true);
    try {
      const report = await runSecurityAudit(slug);
      setSecurityReport(report);
    } catch (err: unknown) {
      console.error("Security audit failed:", err);
      setSecurityReport({
        vulnerabilities: [{
          crate_name: "Cargo.lock Scan",
          version: "N/A",
          advisory_id: "OFFLINE",
          summary: err instanceof ApiError ? err.message : "Security audit service was unreachable. Dependency security scans are offline.",
          severity: "HIGH",
          url: "https://osv.dev/"
        }],
        leaked_secrets: [],
        secure_code_violations: [],
        security_score: 50
      });
    } finally {
      setSecurityLoading(false);
    }
  }

  const handleToggleBreakpoint = useCallback((nodeId: string) => {
    setActiveBreakpoints((prev) => {
      if (prev.includes(nodeId)) {
        return prev.filter((id) => id !== nodeId);
      } else {
        return [...prev, nodeId];
      }
    });
  }, []);

  const displayNodes = useMemo(() => {
    let baseNodes = nodes;
    if (proposedGraph) {
      const rfNodes: Node<NodeData>[] = [];
      const origNodes = graphToShow.nodes;
      const propNodes = proposedGraph.nodes;
      
      const propNodeIds = new Set(propNodes.map(n => n.id));
      
      // 1. Proposed nodes (Added or Modified or Unchanged)
      for (const pn of propNodes) {
        const rfNode = toRfNode(pn);
        const orig = origNodes.find(on => on.id === pn.id);
        if (!orig) {
          // Added
          rfNode.style = {
            border: "2px solid #22c55e",
            boxShadow: "0 0 10px rgba(34, 197, 94, 0.5)",
            background: "rgba(34, 197, 94, 0.05)"
          };
          rfNode.data.label = `🆕 ${rfNode.data.label}`;
        } else if (JSON.stringify(orig.config) !== JSON.stringify(pn.config) || orig.label !== pn.label) {
          // Modified
          rfNode.style = {
            border: "2px solid #eab308",
            boxShadow: "0 0 10px rgba(234, 179, 8, 0.5)",
            background: "rgba(234, 179, 8, 0.05)"
          };
          rfNode.data.label = `📝 ${rfNode.data.label}`;
        }
        rfNodes.push(rfNode);
      }
      
      // 2. Deleted nodes
      for (const on of origNodes) {
        if (!propNodeIds.has(on.id)) {
          const rfNode = toRfNode(on);
          rfNode.style = {
            opacity: 0.5,
            border: "2px dashed #ef4444",
            boxShadow: "0 0 10px rgba(239, 68, 68, 0.2)",
            background: "rgba(239, 68, 68, 0.05)",
            pointerEvents: "none"
          };
          rfNode.data.label = `❌ ${rfNode.data.label} (deleted)`;
          rfNodes.push(rfNode);
        }
      }
      baseNodes = rfNodes;
    }
    
    return baseNodes.map(node => ({
      ...node,
      data: {
        ...node.data,
        isBreakpoint: activeBreakpoints.includes(node.id),
        isPaused: debugPausedNodeId === node.id,
        onToggleBreakpoint: handleToggleBreakpoint,
        metrics: nodeMetrics[node.id],
      }
    }));
  }, [nodes, proposedGraph, graphToShow, activeBreakpoints, debugPausedNodeId, handleToggleBreakpoint, nodeMetrics]);

  const displayEdges = useMemo(() => {
    let baseEdges = edges;
    if (proposedGraph) {
      const rfEdges: Edge[] = [];
      const origEdges = graphToShow.edges;
      const propEdges = proposedGraph.edges;
      
      const propEdgeIds = new Set(propEdges.map(e => e.id));
      
      // 1. Proposed edges
      for (const pe of propEdges) {
        const rfEdge = toRfEdge(pe);
        const orig = origEdges.find(oe => oe.id === pe.id);
        if (!orig) {
          // Added
          rfEdge.style = { stroke: "#22c55e", strokeWidth: 3 };
          rfEdge.animated = true;
        }
        rfEdges.push(rfEdge);
      }
      
      // 2. Deleted edges
      for (const oe of origEdges) {
        if (!propEdgeIds.has(oe.id)) {
          const rfEdge = toRfEdge(oe);
          rfEdge.style = { stroke: "#ef4444", strokeWidth: 2, strokeDasharray: "5,5" };
          rfEdge.animated = false;
          rfEdges.push(rfEdge);
        }
      }
      baseEdges = rfEdges;
    }
    
    return baseEdges.map(edge => {
      const edgeVal = inspectedEdgeValues[edge.id];
      const isHeadingToPaused = edge.target === debugPausedNodeId;
      
      // S21: Look up source node throughput metrics
      const sourceMetrics = nodeMetrics[edge.source];
      const throughput = sourceMetrics?.throughput ?? 0;
      
      const style: React.CSSProperties = {
        ...edge.style,
        transition: "stroke 0.3s, stroke-width 0.3s",
      };
      
      if (isHeadingToPaused) {
        style.stroke = "#06b6d4"; // Vibrant cyan glow
        style.strokeWidth = 3;
      } else if (edgeVal) {
        style.stroke = "#6366f1"; // Indigo wire carrying data
        style.strokeWidth = 2.5;
      } else if (throughput > 0) {
        style.stroke = "#10b981"; // Emerald green wire representing active stream
        style.strokeWidth = 2;
      }
      
      // Determine label: combine edge variable value and edge throughput
      let labelText: string | undefined = undefined;
      if (edgeVal && throughput > 0) {
        labelText = `${edgeVal} (${throughput} ev/s)`;
      } else if (edgeVal) {
        labelText = edgeVal;
      } else if (throughput > 0) {
        labelText = `${throughput} ev/s`;
      }
      
      // S21: Dynamically animate edges based on throughput speed
      const isAnimated = isHeadingToPaused || throughput > 0 || edge.animated;
      
      return {
        ...edge,
        animated: isAnimated,
        style,
        label: labelText,
        labelStyle: {
          fill: "#e2e8f0",
          fontWeight: 600,
          fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
          fontSize: "10px",
        },
        labelBgStyle: {
          fill: "#09090b",
          fillOpacity: 0.95,
          stroke: edgeVal ? "#6366f1" : (throughput > 0 ? "#10b981" : undefined),
          strokeWidth: 1.5,
          rx: 4,
          ry: 4,
        },
        labelBgPadding: [6, 4] as [number, number],
      };
    });
  }, [edges, proposedGraph, graphToShow, inspectedEdgeValues, debugPausedNodeId, nodeMetrics]);

  function handleRejectProposed() {
    setProposedGraph(null);
  }

  async function handleAcceptProposed() {
    if (!proposedGraph) return;
    try {
      await saveGraph(slug, proposedGraph);
      setState({ kind: "ready", graph: proposedGraph });
      setNodes(proposedGraph.nodes.map(toRfNode));
      setEdges(proposedGraph.edges.map(toRfEdge));
      setProposedGraph(null);
      handleBuild(false);
    } catch (err: unknown) {
      const message = err instanceof ApiError ? err.message : "failed to save proposed graph";
      alert(`Failed to accept proposed graph: ${message}`);
    }
  }

  async function handleLlmGenerate() {
    const promptText = aiPrompt.trim();
    if (!promptText || !activeSessionId) return;

    const userMsg: SavedChatMessage = {
      id: `msg_${Date.now()}_user`,
      role: "user",
      content: promptText,
      timestamp: Date.now(),
    };

    // Add user message immediately
    setSessions((prev) =>
      prev.map((s) =>
        s.id === activeSessionId
          ? { ...s, messages: [...s.messages, userMsg] }
          : s
      )
    );

    setAiPrompt("");
    setAiLoading(true);
    setAiError(null);
    setProposedGraph(null);

    // Map history matching ChatMessage API structure
    const historyPayload: ChatMessage[] = activeSession
      ? activeSession.messages.map((m) => ({
          role: m.role,
          content: m.content,
        }))
      : [];

    try {
      const result = await generateFlow(slug, promptText, historyPayload);
      setProposedGraph(result);

      // Add success response from assistant
      const assistantMsg: SavedChatMessage = {
        id: `msg_${Date.now()}_assistant`,
        role: "assistant",
        content: `✨ I have proposed a new visual flow graph based on your request. Please review the highlighted additions (green) and modifications (yellow) on the canvas above.`,
        timestamp: Date.now(),
      };
      setSessions((prev) =>
        prev.map((s) =>
          s.id === activeSessionId
            ? { ...s, messages: [...s.messages, assistantMsg] }
            : s
        )
      );
    } catch (err: unknown) {
      const message = err instanceof ApiError ? err.message : "failed to generate flow graph";
      setAiError(message);

      // Add error response from assistant
      const errorMsg: SavedChatMessage = {
        id: `msg_${Date.now()}_error`,
        role: "assistant",
        content: `⚠️ Failed to generate flow graph: **${message}**`,
        timestamp: Date.now(),
      };
      setSessions((prev) =>
        prev.map((s) =>
          s.id === activeSessionId
            ? { ...s, messages: [...s.messages, errorMsg] }
            : s
        )
      );
    } finally {
      setAiLoading(false);
    }
  }

  async function handleLlmRefine() {
    const promptText = aiPrompt.trim();
    if (!promptText || !activeSessionId) return;

    const userMsg: SavedChatMessage = {
      id: `msg_${Date.now()}_user`,
      role: "user",
      content: promptText,
      timestamp: Date.now(),
    };

    // Add user message immediately
    setSessions((prev) =>
      prev.map((s) =>
        s.id === activeSessionId
          ? { ...s, messages: [...s.messages, userMsg] }
          : s
      )
    );

    setAiPrompt("");
    setAiLoading(true);
    setAiError(null);
    setProposedGraph(null);

    // Map history matching ChatMessage API structure
    const historyPayload: ChatMessage[] = activeSession
      ? activeSession.messages.map((m) => ({
          role: m.role,
          content: m.content,
        }))
      : [];

    try {
      const result = await refineFlow(slug, promptText, historyPayload);
      setProposedGraph(result);

      // Add success response from assistant
      const assistantMsg: SavedChatMessage = {
        id: `msg_${Date.now()}_assistant`,
        role: "assistant",
        content: `🔧 I have refined the visual flow graph based on your request. Please review the highlighted additions (green), changes (yellow), and deletions (red dashed) on the canvas above.`,
        timestamp: Date.now(),
      };
      setSessions((prev) =>
        prev.map((s) =>
          s.id === activeSessionId
            ? { ...s, messages: [...s.messages, assistantMsg] }
            : s
        )
      );
    } catch (err: unknown) {
      const message = err instanceof ApiError ? err.message : "failed to refine flow graph";
      setAiError(message);

      // Add error response from assistant
      const errorMsg: SavedChatMessage = {
        id: `msg_${Date.now()}_error`,
        role: "assistant",
        content: `⚠️ Failed to refine flow graph: **${message}**`,
        timestamp: Date.now(),
      };
      setSessions((prev) =>
        prev.map((s) =>
          s.id === activeSessionId
            ? { ...s, messages: [...s.messages, errorMsg] }
            : s
        )
      );
    } finally {
      setAiLoading(false);
    }
  }

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

  // Sync templates dynamic port information into nodes
  useEffect(() => {
    if (templates.length > 0) {
      setNodes((prevNodes) =>
        prevNodes.map((node) => {
          const template = templates.find((t) => t.id === node.data.templateId);
          return {
            ...node,
            data: {
              ...node.data,
              inputs: template?.input_ports ?? [],
              outputs: template?.output_ports ?? [],
            },
          };
        })
      );
    }
  }, [templates, setNodes]);

  // Sync compiler diagnostics dynamically into nodes in-place without resetting canvas state
  useEffect(() => {
    setNodes((prevNodes) =>
      prevNodes.map((node) => {
        const nodeDiagnostics = diagnostics.filter((d) => d.node_id === node.id);
        return {
          ...node,
          data: {
            ...node.data,
            diagnostics: nodeDiagnostics,
          },
        };
      })
    );
  }, [diagnostics, setNodes]);

  // Build WebSocket
  useEffect(() => {
    const ws = new WebSocket(buildWebSocketUrl(slug));
    wsRef.current = ws;
    ws.onmessage = (event) => {
      const data: BuildEvent = JSON.parse(event.data);
      switch (data.stream) {
        case "start":
          setBuildState({ kind: "running" });
          setDiagnostics([]);
          setBuildLines((prev) => [...prev, `> ${data.command}`]);
          break;
        case "diagnostic":
          if (data.diagnostic) {
            setDiagnostics((prev) => [...prev, data.diagnostic!]);
          }
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

  // Load run status on mount.
  useEffect(() => {
    fetchRunStatus(slug)
      .then((status) => {
        setIsRunning(status.running);
      })
      .catch(() => {});
  }, [slug]);

  // Run WebSocket
  useEffect(() => {
    const ws = new WebSocket(runWebSocketUrl(slug));
    runWsRef.current = ws;
    ws.onmessage = (event) => {
      const data: RunEvent = JSON.parse(event.data);
      switch (data.stream) {
        case "start":
          setIsRunning(true);
          setNodeMetrics({}); // S21: Clear performance stats
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
          setIsRunning(false);
          setDebugPausedNodeId(null);
          setDebugModeActive(false);
          setNodeMetrics({}); // S21: Clear performance stats
          setBuildState({ kind: "idle" });
          setBuildLines((prev) => [...prev, `[exit code ${data.code ?? -1}]`]);
          break;
        case "stop":
          setIsRunning(false);
          setDebugPausedNodeId(null);
          setDebugModeActive(false);
          setNodeMetrics({}); // S21: Clear performance stats
          setBuildState({ kind: "idle" });
          setBuildLines((prev) => [...prev, `[stopped: ${data.reason ?? ""}]`]);
          break;
        case "debug_state" as any: {
          const dbg = data as any;
          const { node_id, state: dbgState, value: dbgVal } = dbg;
          
          if (dbgState === "before" || dbgState === "paused") {
            setEdges((eds) => {
              const incoming = eds.filter((e) => e.target === node_id);
              if (incoming.length > 0) {
                setInspectedEdgeValues((prev) => {
                  const next = { ...prev };
                  incoming.forEach((e) => {
                    next[e.id] = dbgVal;
                  });
                  return next;
                });
              }
              return eds;
            });
            
            if (dbgState === "paused") {
              setDebugPausedNodeId(node_id);
              setDebugModeActive(true);
            }
          } else if (dbgState === "after") {
            setEdges((eds) => {
              const outgoing = eds.filter((e) => e.source === node_id);
              if (outgoing.length > 0) {
                setInspectedEdgeValues((prev) => {
                  const next = { ...prev };
                  outgoing.forEach((e) => {
                    next[e.id] = dbgVal;
                  });
                  return next;
                });
              }
              return eds;
            });
          }
          break;
        }
        case "metrics" as any:
          if (data.metrics) {
            setNodeMetrics(data.metrics);
          }
          break;
      }
    };
    ws.onclose = () => {
      runWsRef.current = null;
    };
    return () => ws.close();
  }, [slug]);

  useEffect(() => {
    linesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [buildLines]);

  // S13: Keyboard shortcuts for debugger
  useEffect(() => {
    if (!debugModeActive || !debugPausedNodeId) return;

    function handleKeyDown(e: KeyboardEvent) {
      if (e.key === "F8") {
        e.preventDefault();
        import("../api").then(({ sendDebugAction }) => {
          sendDebugAction(slug, "resume").catch(() => {});
        });
      } else if (e.key === "F10") {
        e.preventDefault();
        import("../api").then(({ sendDebugAction }) => {
          sendDebugAction(slug, "step").catch(() => {});
        });
      } else if (e.key === "Escape") {
        e.preventDefault();
        handleStop().catch(() => {});
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [debugModeActive, debugPausedNodeId, slug]);

  // Debounced save
  const scheduleSave = useCallback(
    (nextNodes: Node<NodeData>[], nextEdges: Edge[]) => {
      if (isRemoteUpdateRef.current) return;
      if (saveTimerRef.current) {
        window.clearTimeout(saveTimerRef.current);
      }
      saveTimerRef.current = window.setTimeout(() => {
        const graph: Graph = {
          schema_version: 1,
          nodes: nextNodes.map(fromRfNode),
          edges: nextEdges.map(fromRfEdge),
        };
        saveGraph(slug, graph).then(() => {
          if (collabWsRef.current && collabWsRef.current.readyState === WebSocket.OPEN) {
            collabWsRef.current.send(JSON.stringify({
              type: "graph_edit",
              user_id: myCollabUser.id,
              graph: graph,
            }));
          }
        }).catch((err: unknown) => {
          if (err instanceof ApiError) {
            console.error("save failed:", err.message);
          }
        });
      }, 800);
    },
    [slug, myCollabUser]
  );

  useEffect(() => {
    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    const host = window.location.host;
    const wsUrl = `${protocol}//${host}/ws/collab/${slug}`;
    
    const ws = new WebSocket(wsUrl);
    collabWsRef.current = ws;
    
    ws.onopen = () => {
      ws.send(JSON.stringify({
        type: "join",
        user: myCollabUser,
      }));
    };
    
    ws.onmessage = (event) => {
      try {
        const data = JSON.parse(event.data);
        switch (data.type) {
          case "presence": {
            const activeUsers = data.users;
            setCollaborators(activeUsers.filter((u: Collaborator) => u.id !== myCollabUser.id));
            setCursors((current) => {
              const next = { ...current };
              for (const id of Object.keys(next)) {
                if (!activeUsers.some((u: Collaborator) => u.id === id)) {
                  delete next[id];
                }
              }
              return next;
            });
            break;
          }
          case "cursor": {
            const { user_id, x, y } = data;
            setCursors((current) => ({
              ...current,
              [user_id]: { userId: user_id, x, y },
            }));
            break;
          }
          case "node_drag": {
            const { node_id, x, y } = data;
            isRemoteUpdateRef.current = true;
            setNodes((current) =>
              current.map((n) => (n.id === node_id ? { ...n, position: { x, y } } : n))
            );
            setTimeout(() => {
              isRemoteUpdateRef.current = false;
            }, 50);
            break;
          }
          case "graph_edit": {
            const remoteGraph = data.graph;
            isRemoteUpdateRef.current = true;
            setNodes(remoteGraph.nodes.map(toRfNode));
            setEdges(remoteGraph.edges.map(toRfEdge));
            setTimeout(() => {
              isRemoteUpdateRef.current = false;
            }, 50);
            break;
          }
          default:
            break;
        }
      } catch (e) {
        console.error("failed to parse WS message:", e);
      }
    };
    
    return () => {
      ws.close();
      collabWsRef.current = null;
    };
  }, [slug, myCollabUser, setNodes, setEdges]);

  const onCanvasPointerMove = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      const now = Date.now();
      if (now - lastCursorSendRef.current < 40) return;
      lastCursorSendRef.current = now;

      if (!collabWsRef.current || collabWsRef.current.readyState !== WebSocket.OPEN) return;
      const flowPos = screenToFlowPosition({ x: e.clientX, y: e.clientY });
      collabWsRef.current.send(JSON.stringify({
        type: "cursor",
        user_id: myCollabUser.id,
        x: flowPos.x,
        y: flowPos.y,
      }));
    },
    [screenToFlowPosition, myCollabUser]
  );

  const onNodeDrag = useCallback(
    (_event: React.MouseEvent, node: Node) => {
      const now = Date.now();
      if (now - lastCursorSendRef.current < 40) return;
      lastCursorSendRef.current = now;

      if (!collabWsRef.current || collabWsRef.current.readyState !== WebSocket.OPEN) return;
      collabWsRef.current.send(JSON.stringify({
        type: "node_drag",
        user_id: myCollabUser.id,
        node_id: node.id,
        x: node.position.x,
        y: node.position.y,
      }));
    },
    [myCollabUser]
  );

  const onConnect = useCallback(
    (connection: Connection) => {
      if (proposedGraph) return;
      setEdges((eds) => {
        const next = addEdge({ ...connection, id: makeEdgeId() }, eds);
        scheduleSave(nodes, next);
        return next;
      });
    },
    [setEdges, nodes, scheduleSave, proposedGraph]
  );

  const onNodeDoubleClick = useCallback(
    (_event: React.MouseEvent, node: Node<NodeData>) => {
      if (proposedGraph) return;
      setSelectedNodeId(node.id);
    },
    [proposedGraph]
  );

  const onDragOver = useCallback((event: React.DragEvent) => {
    event.preventDefault();
    event.dataTransfer.dropEffect = "move";
  }, []);

  const onDrop = useCallback(
    (event: React.DragEvent) => {
      event.preventDefault();
      if (proposedGraph) return;
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
    [screenToFlowPosition, setNodes, edges, scheduleSave, proposedGraph]
  );

  const wrappedOnNodesChange = useCallback(
    (changes: Parameters<typeof onNodesChange>[0]) => {
      if (proposedGraph) return;
      onNodesChange(changes);
      setNodes((current) => {
        scheduleSave(current, edges);
        return current;
      });
    },
    [onNodesChange, setNodes, edges, scheduleSave, proposedGraph]
  );

  const wrappedOnEdgesChange = useCallback(
    (changes: Parameters<typeof onEdgesChange>[0]) => {
      if (proposedGraph) return;
      onEdgesChange(changes);
      setEdges((current) => {
        scheduleSave(nodes, current);
        return current;
      });
    },
    [onEdgesChange, setEdges, nodes, scheduleSave, proposedGraph]
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
    if (proposedGraph) return;
    setNodes((nds) => {
      const next = nds.map((n) =>
        n.id === nodeId ? { ...n, data: { ...n.data, config } } : n
      );
      scheduleSave(next, edges);
      return next;
    });
  }

  function updateNodeComment(nodeId: string, comment: string) {
    if (proposedGraph) return;
    setNodes((nds) => {
      const next = nds.map((n) =>
        n.id === nodeId ? { ...n, data: { ...n.data, comment } } : n
      );
      scheduleSave(next, edges);
      return next;
    });
  }

  const handleAutoLayout = useCallback(() => {
    if (proposedGraph || nodes.length === 0) return;

    // 1. Identify roots (nodes with no incoming edges or the EntryPoint)
    const incomingCount: Record<string, number> = {};
    nodes.forEach((n) => {
      incomingCount[n.id] = 0;
    });
    edges.forEach((e) => {
      if (incomingCount[e.target] !== undefined) {
        incomingCount[e.target]++;
      }
    });

    const roots = nodes.filter(
      (n) => n.data.templateId === "core.entry_point" || incomingCount[n.id] === 0
    );
    const rootNodes = roots.length > 0 ? roots : [nodes[0]];

    // 2. BFS Traversal to compute hierarchical levels (ranks)
    const levels: Record<string, number> = {};
    nodes.forEach((n) => {
      levels[n.id] = 0;
    });

    const queue: string[] = [];
    rootNodes.forEach((r) => {
      levels[r.id] = 0;
      queue.push(r.id);
    });

    // Build adjacent list representation
    const adj: Record<string, string[]> = {};
    nodes.forEach((n) => {
      adj[n.id] = [];
    });
    edges.forEach((e) => {
      if (adj[e.source]) {
        adj[e.source].push(e.target);
      }
    });

    while (queue.length > 0) {
      const curr = queue.shift()!;
      const currLevel = levels[curr];
      const children = adj[curr] || [];
      
      children.forEach((child) => {
        // Support deep traversal — level of child is max(child_level, parent_level + 1)
        if (levels[child] < currLevel + 1) {
          levels[child] = currLevel + 1;
          queue.push(child);
        }
      });
    }

    // 3. Group nodes by level
    const levelGroups: Record<number, string[]> = {};
    nodes.forEach((n) => {
      const lvl = levels[n.id];
      if (!levelGroups[lvl]) {
        levelGroups[lvl] = [];
      }
      levelGroups[lvl].push(n.id);
    });

    // 4. Position nodes based on Level (X) and Level Index (Y)
    const horizontalSpacing = 280;
    const verticalSpacing = 180;
    const yCenterOffset = 250;

    const nextNodes = nodes.map((node) => {
      const lvl = levels[node.id];
      const group = levelGroups[lvl] || [node.id];
      const index = group.indexOf(node.id);
      const count = group.length;

      // Position
      const x = lvl * horizontalSpacing;
      // Center vertically within column
      const y = (index - (count - 1) / 2) * verticalSpacing + yCenterOffset;

      return {
        ...node,
        position: { x, y },
      };
    });

    // 5. Update state and trigger save
    setNodes(nextNodes);
    scheduleSave(nextNodes, edges);

    // Auto-fit the canvas to show the new layout beautifully
    setTimeout(() => {
      fitView({ duration: 600, padding: 0.15 });
    }, 100);
  }, [nodes, edges, proposedGraph, scheduleSave, setNodes, fitView]);

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

  async function handleRun() {
    setBuildLines([]);
    setBuildState({ kind: "running" });
    try {
      await triggerRun(slug);
    } catch (err: unknown) {
      const message = err instanceof ApiError ? err.message : "failed to start run";
      setBuildState({ kind: "error", message });
      setBuildLines((prev) => [...prev, `[error] ${message}`]);
    }
  }

  async function handleStop() {
    try {
      await stopRun(slug);
    } catch (err: unknown) {
      const message = err instanceof ApiError ? err.message : "failed to stop run";
      setBuildLines((prev) => [...prev, `[error] ${message}`]);
    }
  }

  async function handleTest() {
    setBuildLines([]);
    setBuildState({ kind: "running" });
    try {
      await triggerTest(slug);
    } catch (err: unknown) {
      const message = err instanceof ApiError ? err.message : "failed to start test";
      setBuildState({ kind: "error", message });
      setBuildLines((prev) => [...prev, `[error] ${message}`]);
    }
  }

  async function handleDebug() {
    setBuildLines([]);
    setBuildState({ kind: "running" });
    setInspectedEdgeValues({});
    setDebugPausedNodeId(null);
    setNodeMetrics({}); // S21: Clear performance stats
    setDebugModeActive(true);
    try {
      await triggerDebug(slug, activeBreakpoints);
    } catch (err: unknown) {
      const message = err instanceof ApiError ? err.message : "failed to start debug";
      setBuildState({ kind: "error", message });
      setBuildLines((prev) => [...prev, `[error] ${message}`]);
      setDebugModeActive(false);
    }
  }

  return (
    <div className="project-canvas">
      <header className="project-canvas-header">
        <button type="button" onClick={onBack}>← projects</button>
        <span className="slug-badge"><code>{slug}</code></span>
        <span className="muted">
          {graphToShow.nodes.length} nodes · {graphToShow.edges.length} edges
        </span>
        <div className="collaborators-bar" style={{ display: "flex", alignItems: "center", gap: "6px", marginLeft: "auto", marginRight: "12px" }}>
          {collaborators.map((user) => {
            const initials = user.username.split("-")[0].substring(0, 2).toUpperCase();
            return (
              <div
                key={user.id}
                className="collaborator-avatar"
                style={{
                  width: "28px",
                  height: "28px",
                  borderRadius: "50%",
                  backgroundColor: user.color,
                  color: "#0f172a",
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  fontSize: "11px",
                  fontWeight: "bold",
                  border: "2px solid #1e293b",
                  boxShadow: `0 0 8px ${user.color}80`,
                }}
                title={user.username}
              >
                {initials}
              </div>
            );
          })}
          <div
            className="collaborator-avatar current"
            style={{
              width: "28px",
              height: "28px",
              borderRadius: "50%",
              backgroundColor: myCollabUser.color,
              color: "#0f172a",
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              fontSize: "11px",
              fontWeight: "bold",
              border: "2px solid #38bdf8",
              boxShadow: `0 0 8px ${myCollabUser.color}cc`,
            }}
            title={`${myCollabUser.username} (You)`}
          >
            {myCollabUser.username.split("-")[0].substring(0, 2).toUpperCase()}
          </div>
        </div>
        <button type="button" className="btn-auto-layout" onClick={handleAutoLayout}>
          🪄 Auto-Layout
        </button>
      </header>

      {proposedGraph && (
        <div className="proposed-banner">
          <div className="proposed-banner-text">
            <span>✨ <strong>AI Proposed Flow:</strong> Review the highlighted changes on the canvas below before accepting or rejecting.</span>
          </div>
          <div className="proposed-banner-actions">
            <button type="button" className="btn-accept" onClick={handleAcceptProposed}>Accept Proposed Flow</button>
            <button type="button" className="btn-reject" onClick={handleRejectProposed}>Reject / Cancel</button>
          </div>
        </div>
      )}

      {state.kind === "loading" && <p className="muted center">loading graph…</p>}
      {state.kind === "error" && <p className="form-error center">failed to load: {state.message}</p>}

      {state.kind === "ready" && (
        <div className="project-canvas-body">
          <aside className="project-sidebar">
            <NodePalette />

            <div className="ai-chat-container">
              <div className="ai-chat-header">
                <div className="ai-chat-title-group">
                  <h3>AI Chat Assistant</h3>
                </div>
                <div className="ai-session-controls">
                  <button
                    type="button"
                    className="btn-new-session"
                    onClick={handleNewSession}
                    title="Create a new design session"
                  >
                    + New Chat
                  </button>
                  {activeSession && (
                    <button
                      type="button"
                      className="btn-delete-session"
                      onClick={() => handleDeleteSession(activeSession.id)}
                      title="Clear messages or delete session"
                    >
                      Delete
                    </button>
                  )}
                </div>
              </div>

              {sessions.length > 0 && activeSessionId && (
                <div className="ai-session-bar">
                  {isRenamingSession ? (
                    <input
                      type="text"
                      className="ai-session-rename-input"
                      value={renameTitle}
                      onChange={(e) => setRenameTitle(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") handleRenameSession();
                        if (e.key === "Escape") setIsRenamingSession(false);
                      }}
                      autoFocus
                      onBlur={handleRenameSession}
                    />
                  ) : (
                    <select
                      className="ai-session-select"
                      value={activeSessionId}
                      onChange={(e) => {
                        setActiveSessionId(e.target.value);
                        setIsRenamingSession(false);
                      }}
                    >
                      {sessions.map((s) => (
                        <option key={s.id} value={s.id}>
                          {s.title}
                        </option>
                      ))}
                    </select>
                  )}
                  <button
                    type="button"
                    className="btn-rename-toggle"
                    onClick={() => {
                      if (isRenamingSession) {
                        handleRenameSession();
                      } else if (activeSession) {
                        setRenameTitle(activeSession.title);
                        setIsRenamingSession(true);
                      }
                    }}
                  >
                    {isRenamingSession ? "Save" : "✏️"}
                  </button>
                </div>
              )}

              <div className="ai-chat-messages">
                {activeSession && activeSession.messages.length === 0 ? (
                  <div className="ai-chat-empty">
                    <span className="sparkle">✨</span>
                    <p className="muted" style={{ fontWeight: 600, color: "#818cf8", margin: 0 }}>
                      AI Flow Designer
                    </p>
                    <p className="muted" style={{ fontSize: "0.75rem", margin: 0 }}>
                      Describe what visual nodes or pipelines you want to build. Claude will design the flow!
                    </p>
                  </div>
                ) : (
                  activeSession?.messages.map((m) => (
                    <div key={m.id} className={`ai-chat-bubble ${m.role}`}>
                      {m.content}
                    </div>
                  ))
                )}

                {aiLoading && (
                  <div className="ai-chat-bubble-loading">
                    <span className="muted" style={{ fontSize: "0.72rem" }}>Claude is thinking</span>
                    <div className="ai-loading-dots">
                      <span></span>
                      <span></span>
                      <span></span>
                    </div>
                  </div>
                )}
                <div ref={chatMessagesEndRef} />
              </div>

              {aiError && <p className="form-error" style={{ margin: 0, fontSize: "0.75rem" }}>{aiError}</p>}

              <div className="ai-chat-input-area">
                <textarea
                  className="ai-chat-textarea"
                  value={aiPrompt}
                  onChange={(e) => setAiPrompt(e.target.value)}
                  placeholder="e.g., Create a cron scheduler polling file /var/log/syslog, parsing to JSON, and writing to Kafka producer..."
                  disabled={aiLoading}
                  rows={3}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      handleLlmGenerate();
                    }
                  }}
                />
                <div className="ai-actions">
                  <button
                    type="button"
                    className="btn-ai-generate"
                    onClick={handleLlmGenerate}
                    disabled={aiLoading || !aiPrompt.trim()}
                    title="Generate a proposed flow from scratch or overwrite current"
                  >
                    {aiLoading ? "Thinking..." : "Generate Flow"}
                  </button>
                  <button
                    type="button"
                    className="btn-ai-refine"
                    onClick={handleLlmRefine}
                    disabled={aiLoading || !aiPrompt.trim()}
                    title="Refine the current canvas layout with edits"
                  >
                    {aiLoading ? "Thinking..." : "Refine Flow"}
                  </button>
                </div>
              </div>
            </div>

            <div className="build-panel">
              <h3>Execution</h3>
              <div className="build-actions">
                <button
                  type="button"
                  onClick={() => handleBuild(false)}
                  disabled={buildState.kind === "running" || isRunning}
                >
                  Check
                </button>
                <button
                  type="button"
                  onClick={() => handleBuild(true)}
                  disabled={buildState.kind === "running" || isRunning}
                >
                  Build Release
                </button>
              </div>
              <div className="build-actions">
                <button
                  type="button"
                  onClick={handleTest}
                  disabled={buildState.kind === "running" || isRunning}
                >
                  Test
                </button>
                {isRunning ? (
                  <button
                    type="button"
                    onClick={handleStop}
                    className="danger"
                  >
                    Stop
                  </button>
                ) : (
                  <>
                    <button
                      type="button"
                      onClick={handleRun}
                      disabled={buildState.kind === "running"}
                    >
                      Run
                    </button>
                    <button
                      type="button"
                      onClick={handleDebug}
                      disabled={buildState.kind === "running"}
                    >
                      Debug
                    </button>
                  </>
                )}
              </div>
              <div className="build-actions" style={{ marginTop: "0.5rem" }}>
                <button
                  type="button"
                  onClick={handleSecurityAudit}
                  className="btn-security-audit"
                >
                  🛡️ Security Audit
                </button>
              </div>
              <div style={{ display: "flex", gap: "0.5rem", alignItems: "center" }}>
                <BuildBadge state={buildState} />
                {isRunning && <span className="badge badge-ok">running</span>}
              </div>
              <div className="build-output">
                {buildLines.map((line, i) => (
                  <pre key={i}>{line}</pre>
                ))}
                <div ref={linesEndRef} />
              </div>
            </div>
          </aside>

          <main className="studio-canvas" style={{ position: "relative" }} onPointerMove={onCanvasPointerMove}>
            <ReactFlow<Node<NodeData>>
              nodes={displayNodes}
              edges={displayEdges}
              nodeTypes={{ entryPoint: EntryPointNode, studio: StudioNode }}
              onNodesChange={wrappedOnNodesChange}
              onEdgesChange={wrappedOnEdgesChange}
              onConnect={onConnect}
              onNodeDoubleClick={onNodeDoubleClick}
              onDragOver={onDragOver}
              onDrop={onDrop}
              onNodeDrag={onNodeDrag}
              fitView
              nodesDraggable={!proposedGraph}
              nodesConnectable={!proposedGraph}
              elementsSelectable={!proposedGraph}
              deleteKeyCode={proposedGraph ? [] : ["Backspace", "Delete"]}
            >
              <Background />
              <Controls />
              <MiniMap pannable zoomable />
            </ReactFlow>

            <CursorsOverlay cursors={cursors} collaborators={collaborators} />

            {debugModeActive && (
              <div className="premium-debugger-bar">
                <div className="debugger-status">
                  <span className={`pulse-indicator ${debugPausedNodeId ? "paused" : "running"}`}></span>
                  {debugPausedNodeId ? (
                    <span>Paused at Node: <strong>{displayNodes.find(n => n.id === debugPausedNodeId)?.data.label || debugPausedNodeId}</strong></span>
                  ) : (
                    <span>Debugger Executing...</span>
                  )}
                </div>
                <div className="debugger-divider"></div>
                <div className="debugger-controls">
                  <button
                    type="button"
                    className="btn-control resume"
                    disabled={!debugPausedNodeId}
                    onClick={() => import("../api").then(({ sendDebugAction }) => sendDebugAction(slug, "resume"))}
                    title="Resume / Continue (F8)"
                  >
                    ▶ Resume
                  </button>
                  <button
                    type="button"
                    className="btn-control step"
                    disabled={!debugPausedNodeId}
                    onClick={() => import("../api").then(({ sendDebugAction }) => sendDebugAction(slug, "step"))}
                    title="Step Over (F10)"
                  >
                    ➔ Step Over
                  </button>
                  <button
                    type="button"
                    className="btn-control stop"
                    onClick={handleStop}
                    title="Stop (Esc)"
                  >
                    ■ Stop
                  </button>
                </div>
              </div>
            )}
          </main>

          {selectedNode && (
            <NodeConfigDrawer
              slug={slug || ""}
              nodeId={selectedNode.id}
              templateId={selectedNode.data.templateId}
              config={selectedNode.data.config}
              comment={selectedNode.data.comment || ""}
              onCommentChange={(comment) => updateNodeComment(selectedNode.id, comment)}
              templates={templates}
              diagnostics={diagnostics.filter((d) => d.node_id === selectedNode.id)}
              onChange={(config) => updateNodeConfig(selectedNode.id, config)}
              onClose={() => setSelectedNodeId(null)}
            />
          )}
 
          <SecurityDrawer
            isOpen={isSecurityOpen}
            onClose={() => setIsSecurityOpen(false)}
            report={securityReport}
            isRunning={securityLoading}
            onNodeFocus={handleNodeFocus}
          />
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
