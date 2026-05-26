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
  loadPackageGraph,
  savePackageGraph,
  listPackages,
  createPackage,
  renamePackage,
  deletePackage,
  type Package,
  buildWebSocketUrl,
  collabWebSocketUrl,
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
  type PerformanceStats,
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
import { PackageTree } from "./PackageTree";

/// Slug of the root package — must match `backend::projects::types::ROOT_PACKAGE_SLUG`.
const ROOT_PACKAGE_SLUG = "main";


const MARKETPLACE_CATALOG = [
  { id: "scylla", name: "ScyllaDB NoSQL", icon: "⚡", crate: "scylla @ 0.10.0", desc: "High-performance, low-latency C++ Cassandra-compatible NoSQL database.", category: "Database" },
  { id: "mongodb", name: "MongoDB Client", icon: "🍃", crate: "mongodb @ 2.8.0", desc: "Enterprise document-store NoSQL database utilizing BSON document filtering.", category: "Database" },
  { id: "nats", name: "NATS PubSub", icon: "🌐", crate: "async-nats @ 0.33.0", desc: "Cloud-native, ultra-high performance lightweight subscription & messaging queue.", category: "PubSub" },
  { id: "surrealdb", name: "SurrealDB Multi-Model", icon: "🔮", crate: "surrealdb @ 1.0.0", desc: "Scalable developer-friendly multi-model relational graph database.", category: "Database" },
  { id: "clickhouse", name: "ClickHouse Analytics", icon: "📊", crate: "clickhouse @ 0.11.0", desc: "Column-oriented analytical DBMS enabling real-time big-data aggregates.", category: "Analytics" },
  { id: "s3", name: "AWS S3 Storage", icon: "📦", crate: "aws-sdk-s3 @ 1.0.0", desc: "Scalable cloud-based object storage for file streams and metadata blobs.", category: "Cloud" },
  { id: "webrtc", name: "WebRTC Channels", icon: "📞", crate: "webrtc @ 0.10.0", desc: "Real-time communication, audio, video and binary peer transports.", category: "Network" },
  { id: "rabbitmq", name: "RabbitMQ (AMQP)", icon: "🐇", crate: "lapin @ 0.15.0", desc: "Robust and reliable AMQP-protocol message queue broker client.", category: "PubSub" },
];

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
  comment?: string;
  diagnostics?: any[];
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
        ? ((gn.config as any)?.inputs || []).map((p: any) => ({
            name: p.name,
            type_tag: p.ty,
            multiplicity: "single",
            doc: `Custom parameter ${p.name} of type ${p.ty}`,
          }))
        : template?.input_ports ?? [],
      outputs: (templateId === "custom.block" || templateId === "grpc.server")
        ? ((gn.config as any)?.outputs || []).map((p: any) => ({
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

  // T5: package-tree state. `currentPackage` is the slug whose graph is
  // currently being edited on the canvas. Switching it reloads the
  // graph from `/packages/<slug>/graph`. `packages` is the flat list
  // returned by the backend's `list_packages` endpoint; the sidebar
  // re-derives the tree shape from it.
  const [currentPackage, setCurrentPackage] = useState<string>(ROOT_PACKAGE_SLUG);
  const [packages, setPackages] = useState<Package[]>([]);
  const [packageError, setPackageError] = useState<string | null>(null);

  const [nodes, setNodes, onNodesChange] = useNodesState<Node<NodeData>>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>([]);
  const { screenToFlowPosition, setCenter, fitView } = useReactFlow();

  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);

  const [buildState, setBuildState] = useState<BuildState>({ kind: "idle" });
  const [buildLines, setBuildLines] = useState<string[]>([]);
  const [isRunning, setIsRunning] = useState<boolean>(false);
  const wsRef = useRef<WebSocket | null>(null);
  const runWsRef = useRef<WebSocket | null>(null);
  const linesEndRef = useRef<HTMLDivElement | null>(null);

  const saveTimerRef = useRef<number | null>(null);
  const [saveStatus, setSaveStatus] = useState<'saved' | 'saving' | 'unsaved' | 'error'>('saved');
  const [isConsoleCollapsed, setIsConsoleCollapsed] = useState<boolean>(false);

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

  // LLM Selector states (secure, scoped per-provider client API key storage)
  const [showLlmSettings, setShowLlmSettings] = useState(false);
  const [llmProvider, setLlmProvider] = useState<string>(() => {
    return localStorage.getItem("rust_builder_llm_provider") || "claude_cli";
  });
  const [llmApiKey, setLlmApiKey] = useState<string>("");
  const [llmModel, setLlmModel] = useState<string>("");

  const DEFAULT_MODELS: Record<string, string> = {
    claude_cli: "claude",
    anthropic: "claude-3-5-sonnet-latest",
    open_ai: "gpt-4o",
    deep_seek: "deepseek-chat",
    kimi: "moonshot-v1-8k",
    codex: "gpt-4o"
  };

  useEffect(() => {
    localStorage.setItem("rust_builder_llm_provider", llmProvider);
    const storedKey = localStorage.getItem(`rust_builder_llm_api_key_${llmProvider}`) || "";
    const storedModel = localStorage.getItem(`rust_builder_llm_model_${llmProvider}`) || DEFAULT_MODELS[llmProvider] || "";
    setLlmApiKey(storedKey);
    setLlmModel(storedModel);
  }, [llmProvider]);

  const saveLlmCredentials = (key: string, model: string) => {
    setLlmApiKey(key);
    setLlmModel(model);
    localStorage.setItem(`rust_builder_llm_api_key_${llmProvider}`, key);
    localStorage.setItem(`rust_builder_llm_model_${llmProvider}`, model);
  };

  // Auto-scroll chat to bottom
  useEffect(() => {
    chatMessagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [activeSession?.messages, aiLoading]);

  // Marketplace S22 state & actions
  const [installedMarketplace, setInstalledMarketplace] = useState<string[]>([]);
  const [sidebarTab, setSidebarTab] = useState<"nodes" | "marketplace">("nodes");

  useEffect(() => {
    let active = true;
    import("../api").then(({ fetchMarketplace }) => {
      fetchMarketplace(slug)
        .then((pkgs) => {
          if (active) setInstalledMarketplace(pkgs);
        })
        .catch((err) => console.error("Failed to load marketplace installed packages", err));
    });
    return () => { active = false; };
  }, [slug]);

  async function handleToggleInstall(packageId: string, isInstalled: boolean) {
    const api = await import("../api");
    try {
      let pkgs: string[];
      if (isInstalled) {
        pkgs = await api.uninstallMarketplacePackage(slug, packageId);
        setBuildLines((prev) => [...prev, `[system] Uninstalled marketplace package: ${packageId}`]);
      } else {
        pkgs = await api.installMarketplacePackage(slug, packageId);
        setBuildLines((prev) => [...prev, `[system] Installed marketplace package: ${packageId}`]);
      }
      setInstalledMarketplace(pkgs);
    } catch (err: unknown) {
      const msg = err instanceof api.ApiError ? err.message : String(err);
      setBuildLines((prev) => [...prev, `[error] Marketplace action failed: ${msg}`]);
    }
  }

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
      await savePackageGraph(slug, currentPackage, proposedGraph);
      setState({ kind: "ready", graph: proposedGraph });
      setNodes(proposedGraph.nodes.map((node) => toRfNode(node, templates, diagnostics)));
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
      const result = await generateFlow(
        slug,
        promptText,
        historyPayload,
        llmProvider,
        llmApiKey || undefined,
        llmModel || undefined
      );
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
      const result = await refineFlow(
        slug,
        promptText,
        historyPayload,
        llmProvider,
        llmApiKey || undefined,
        llmModel || undefined
      );
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

  // Load graph on mount AND whenever the user switches the active
  // package via the sidebar. The legacy `loadGraph` shim still works
  // for the root package, but going through the per-package endpoint
  // uniformly means children just work without a branch.
  useEffect(() => {
    const controller = new AbortController();
    setState({ kind: "loading" });
    loadPackageGraph(slug, currentPackage, controller.signal)
      .then((graph) => {
        setState({ kind: "ready", graph });
        setNodes(graph.nodes.map((node) => toRfNode(node, templates, diagnostics)));
        setEdges(graph.edges.map(toRfEdge));
      })
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === "AbortError") return;
        // A package created via T3 may not yet have a graph file on
        // disk; the backend returns 404 in that case. Treat it as an
        // empty canvas rather than an error — the next save creates
        // the file.
        if (err instanceof ApiError && err.code === "not_found") {
          setState({ kind: "ready", graph: EMPTY_GRAPH });
          setNodes([]);
          setEdges([]);
          return;
        }
        const message = err instanceof ApiError ? err.message : "unknown error";
        setState({ kind: "error", message });
      });
    return () => controller.abort();
  }, [slug, currentPackage, setNodes, setEdges]);

  // Load package list on mount + whenever `slug` changes. Kept
  // separate from the graph effect so a per-package switch (cheap)
  // doesn't refetch the tree.
  useEffect(() => {
    const controller = new AbortController();
    listPackages(slug, controller.signal)
      .then(setPackages)
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === "AbortError") return;
        // Don't blow up the whole canvas if the package list fails —
        // surface inline in the sidebar and let the user retry.
        const message = err instanceof ApiError ? err.message : "unknown error";
        setPackageError(`Failed to load packages: ${message}`);
      });
    return () => controller.abort();
  }, [slug]);

  // Package-tree action handlers. Each call optimistically refetches
  // the list on success so the sidebar reflects the new server state
  // without manual array manipulation. If the chosen package is
  // deleted, fall back to root.
  const refreshPackages = useCallback(async () => {
    try {
      const next = await listPackages(slug);
      setPackages(next);
      setPackageError(null);
    } catch (err) {
      const message = err instanceof ApiError ? err.message : "unknown error";
      setPackageError(message);
    }
  }, [slug]);

  const handleCreatePackage = useCallback(
    async (parentId: string | null, newSlug: string) => {
      try {
        await createPackage(slug, {
          slug: newSlug,
          parent_id: parentId ?? undefined,
        });
        await refreshPackages();
      } catch (err) {
        const message = err instanceof ApiError ? err.message : "unknown error";
        setPackageError(`Create failed: ${message}`);
      }
    },
    [slug, refreshPackages],
  );

  const handleRenamePackage = useCallback(
    async (pkgSlug: string, newSlug: string) => {
      if (pkgSlug === newSlug) return;
      try {
        await renamePackage(slug, pkgSlug, { slug: newSlug });
        await refreshPackages();
        // If we just renamed the currently-selected package, follow
        // it to the new slug so the canvas doesn't try to load a
        // stale path.
        if (currentPackage === pkgSlug) {
          setCurrentPackage(newSlug);
        }
      } catch (err) {
        const message = err instanceof ApiError ? err.message : "unknown error";
        setPackageError(`Rename failed: ${message}`);
      }
    },
    [slug, currentPackage, refreshPackages],
  );

  const handleDeletePackage = useCallback(
    async (pkgSlug: string) => {
      try {
        await deletePackage(slug, pkgSlug);
        // If the deleted package was selected (or an ancestor of the
        // selected one), fall back to root.
        if (currentPackage === pkgSlug) {
          setCurrentPackage(ROOT_PACKAGE_SLUG);
        }
        await refreshPackages();
      } catch (err) {
        const message = err instanceof ApiError ? err.message : "unknown error";
        setPackageError(`Delete failed: ${message}`);
      }
    },
    [slug, currentPackage, refreshPackages],
  );

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

  // Debounced save (5 seconds delay)
  const scheduleSave = useCallback(
    (nextNodes: Node<NodeData>[], nextEdges: Edge[]) => {
      if (isRemoteUpdateRef.current) return;
      setSaveStatus("unsaved");
      if (saveTimerRef.current) {
        window.clearTimeout(saveTimerRef.current);
      }
      saveTimerRef.current = window.setTimeout(() => {
        setSaveStatus("saving");
        const graph: Graph = {
          schema_version: 1,
          nodes: nextNodes.map(fromRfNode),
          edges: nextEdges.map(fromRfEdge),
        };
        savePackageGraph(slug, currentPackage, graph).then(() => {
          setSaveStatus("saved");
          if (collabWsRef.current && collabWsRef.current.readyState === WebSocket.OPEN) {
            collabWsRef.current.send(JSON.stringify({
              type: "graph_edit",
              user_id: myCollabUser.id,
              graph: graph,
            }));
          }
        }).catch((err: unknown) => {
          setSaveStatus("error");
          if (err instanceof ApiError) {
            console.error("save failed:", err.message);
          }
        });
      }, 5000); // Auto-save after exactly 5 seconds of inactivity
    },
    [slug, myCollabUser]
  );

  // Manual save trigger (immediate)
  const handleSave = useCallback(async () => {
    if (isRemoteUpdateRef.current) return;
    if (saveTimerRef.current) {
      window.clearTimeout(saveTimerRef.current);
    }
    setSaveStatus("saving");
    try {
      const graph: Graph = {
        schema_version: 1,
        nodes: nodes.map(fromRfNode),
        edges: edges.map(fromRfEdge),
      };
      await savePackageGraph(slug, currentPackage, graph);
      setSaveStatus("saved");
      if (collabWsRef.current && collabWsRef.current.readyState === WebSocket.OPEN) {
        collabWsRef.current.send(JSON.stringify({
          type: "graph_edit",
          user_id: myCollabUser.id,
          graph: graph,
        }));
      }
    } catch (err: unknown) {
      setSaveStatus("error");
      if (err instanceof ApiError) {
        console.error("save failed:", err.message);
      }
    }
  }, [slug, nodes, edges, myCollabUser]);

  useEffect(() => {
    const wsUrl = collabWebSocketUrl(slug);
    
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
             setNodes(remoteGraph.nodes.map((node: GraphNode) => toRfNode(node, templates, diagnostics)));
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

      const template = templates.find((t) => t.id === data.templateId);
      const newNode: Node<NodeData> = {
        id: makeNodeId(),
        type: data.templateId === "core.entry_point" ? "entryPoint" : "studio",
        position,
        data: {
          label: template?.display.name ?? data.templateId,
          templateId: data.templateId,
          config: data.defaultConfig ?? {},
          inputs: template?.input_ports ?? [],
          outputs: template?.output_ports ?? [],
          diagnostics: [],
          comment: "",
        },
      };

      setNodes((nds) => {
        const next = [...nds, newNode];
        scheduleSave(next, edges);
        return next;
      });
    },
    [screenToFlowPosition, setNodes, edges, scheduleSave, proposedGraph, templates]
  );

  const wrappedOnNodesChange = useCallback(
    (changes: Parameters<typeof onNodesChange>[0]) => {
      if (proposedGraph) return;
      onNodesChange(changes);

      // Only save immediately on structural removals (Backspace/Delete)
      const hasRemove = changes.some((c) => c.type === "remove");
      if (hasRemove) {
        setNodes((current) => {
          scheduleSave(current, edges);
          return current;
        });
      }
    },
    [onNodesChange, setNodes, edges, scheduleSave, proposedGraph]
  );

  const wrappedOnEdgesChange = useCallback(
    (changes: Parameters<typeof onEdgesChange>[0]) => {
      if (proposedGraph) return;
      onEdgesChange(changes);

      const hasRemove = changes.some((c) => c.type === "remove");
      if (hasRemove) {
        setEdges((current) => {
          scheduleSave(nodes, current);
          return current;
        });
      }
    },
    [onEdgesChange, setEdges, nodes, scheduleSave, proposedGraph]
  );

  const onNodeDragStop = useCallback(
    (_event: React.MouseEvent, _node: Node) => {
      if (proposedGraph) return;
      // Dragging has finished: save final coordinates from latest render state
      scheduleSave(nodes, edges);
    },
    [nodes, edges, scheduleSave, proposedGraph]
  );

  const deleteNode = useCallback(
    (nodeId: string) => {
      if (proposedGraph) return;
      if (!window.confirm("Are you sure you want to delete this node and all its connected edges?")) return;

      setNodes((nds) => {
        const nextNodes = nds.filter((n) => n.id !== nodeId);
        setEdges((eds) => {
          const nextEdges = eds.filter((e) => e.source !== nodeId && e.target !== nodeId);
          scheduleSave(nextNodes, nextEdges);
          return nextEdges;
        });
        return nextNodes;
      });
      setSelectedNodeId(null);
    },
    [setNodes, setEdges, scheduleSave, proposedGraph]
  );

  const deleteEdge = useCallback(
    (edgeId: string) => {
      if (proposedGraph) return;
      setEdges((eds) => {
        const next = eds.filter((e) => e.id !== edgeId);
        scheduleSave(nodes, next);
        return next;
      });
    },
    [nodes, setEdges, scheduleSave, proposedGraph]
  );

  const selectedNode = useMemo(
    () => nodes.find((n) => n.id === selectedNodeId) ?? null,
    [nodes, selectedNodeId]
  );

  const selectedEdge = useMemo(
    () => edges.find((e) => e.selected) ?? null,
    [edges]
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
    setIsConsoleCollapsed(false);
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
    setIsConsoleCollapsed(false);
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
    setIsConsoleCollapsed(false);
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
    setIsConsoleCollapsed(false);
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
        {selectedEdge && (
          <button
            type="button"
            className="btn-delete-edge"
            onClick={() => deleteEdge(selectedEdge.id)}
            style={{
              padding: "0.4rem 0.75rem",
              borderRadius: "6px",
              border: "1px solid #f43f5e",
              background: "rgba(244, 63, 94, 0.15)",
              color: "#fb7185",
              fontSize: "0.78rem",
              fontWeight: 600,
              cursor: "pointer",
              transition: "all 0.2s",
              display: "flex",
              alignItems: "center",
              gap: "0.35rem",
              marginLeft: "auto",
              marginRight: "10px",
              boxShadow: "0 0 10px rgba(244, 63, 94, 0.2)"
            }}
            onMouseEnter={(e) => {
              e.currentTarget.style.background = "#f43f5e";
              e.currentTarget.style.color = "white";
            }}
            onMouseLeave={(e) => {
              e.currentTarget.style.background = "rgba(244, 63, 94, 0.15)";
              e.currentTarget.style.color = "#fb7185";
            }}
          >
            🗑️ Delete Edge
          </button>
        )}
        {/* S20 IDE Style Run/Debug/Build Controls */}
        <div className="ide-controls-group" style={{ display: "flex", alignItems: "center", gap: "6px", marginLeft: "12px" }}>
          <button
            type="button"
            className="btn-ide-action btn-check"
            onClick={() => handleBuild(false)}
            disabled={buildState.kind === "running" || isRunning}
            title="Run cargo check to validate visual flow schemas and types"
          >
            🔎 Check
          </button>
          
          <button
            type="button"
            className="btn-ide-action btn-build-release"
            onClick={() => handleBuild(true)}
            disabled={buildState.kind === "running" || isRunning}
            title="Compile optimized standalone binary for deployment"
          >
            📦 Release Build
          </button>

          <button
            type="button"
            className="btn-ide-action btn-test"
            onClick={handleTest}
            disabled={buildState.kind === "running" || isRunning}
            title="Execute cargo test suite to verify handlers and dataflows"
          >
            🧪 Test
          </button>

          <span className="ide-control-divider" style={{ borderLeft: "1px solid rgba(255,255,255,0.15)", height: "16px", margin: "0 4px" }} />

          {isRunning ? (
            <button
              type="button"
              className="btn-ide-action btn-stop danger"
              onClick={handleStop}
              title="Stop running service subprocess"
            >
              🛑 Stop
            </button>
          ) : (
            <button
              type="button"
              className="btn-ide-action btn-run success"
              onClick={handleRun}
              disabled={buildState.kind === "running"}
              title="Boot compile-generation and launch service locally"
            >
              ▶️ Run
            </button>
          )}

          <button
            type="button"
            className="btn-ide-action btn-debug warning"
            onClick={handleDebug}
            disabled={buildState.kind === "running" || isRunning}
            title="Launch dynamic step debugger with active breakpoints"
          >
            🪲 Debug
          </button>

          <span className="ide-control-divider" style={{ borderLeft: "1px solid rgba(255,255,255,0.15)", height: "16px", margin: "0 4px" }} />

          <button
            type="button"
            className="btn-ide-action btn-security"
            onClick={handleSecurityAudit}
            disabled={securityLoading}
            title="Execute RUSTSEC audit to scan dependencies and secret leaks"
          >
            {securityLoading ? "🛡️ Auditing..." : "🛡️ Security"}
          </button>
        </div>

        {/* S20 Premium Save Status & Manual Save Badge */}
        <div className="save-status-container" style={{ display: "flex", alignItems: "center", gap: "8px", marginLeft: selectedEdge ? "0" : "auto" }}>
          <span className={`save-status-badge ${saveStatus}`} title="Saves automatically after 5 seconds of inactivity">
            {saveStatus === "saved" && "🟢 Auto-saved"}
            {saveStatus === "saving" && "🟡 Saving..."}
            {saveStatus === "unsaved" && "⏳ Unsaved changes"}
            {saveStatus === "error" && "🔴 Save failed"}
          </span>
          <button
            type="button"
            className="btn-manual-save"
            onClick={handleSave}
            disabled={saveStatus === "saving"}
            title="Save changes immediately"
          >
            💾 Save
          </button>
        </div>

        <button type="button" className="btn-auto-layout" onClick={handleAutoLayout} style={{ marginLeft: "10px" }}>
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
          <PackageTree
            packages={packages}
            selectedSlug={currentPackage}
            onSelect={setCurrentPackage}
            onCreate={handleCreatePackage}
            onRename={handleRenamePackage}
            onDelete={handleDeletePackage}
            errorMessage={packageError}
          />
          <aside className="project-sidebar">
            <div className="sidebar-tab-header" style={{ display: "flex", borderBottom: "1px solid rgba(255,255,255,0.08)", background: "rgba(0,0,0,0.15)" }}>
              <button
                type="button"
                className={`sidebar-tab-btn ${sidebarTab === "nodes" ? "active" : ""}`}
                onClick={() => setSidebarTab("nodes")}
                style={{
                  flex: 1,
                  padding: "0.6rem 0.8rem",
                  background: sidebarTab === "nodes" ? "rgba(99, 102, 241, 0.12)" : "transparent",
                  color: sidebarTab === "nodes" ? "#818cf8" : "rgba(255,255,255,0.6)",
                  border: "none",
                  borderBottom: sidebarTab === "nodes" ? "2px solid #6366f1" : "none",
                  fontWeight: 600,
                  fontSize: "0.8rem",
                  cursor: "pointer",
                  transition: "all 0.2s"
                }}
              >
                🧩 Nodes
              </button>
              <button
                type="button"
                className={`sidebar-tab-btn ${sidebarTab === "marketplace" ? "active" : ""}`}
                onClick={() => setSidebarTab("marketplace")}
                style={{
                  flex: 1,
                  padding: "0.6rem 0.8rem",
                  background: sidebarTab === "marketplace" ? "rgba(99, 102, 241, 0.12)" : "transparent",
                  color: sidebarTab === "marketplace" ? "#818cf8" : "rgba(255,255,255,0.6)",
                  border: "none",
                  borderBottom: sidebarTab === "marketplace" ? "2px solid #6366f1" : "none",
                  fontWeight: 600,
                  fontSize: "0.8rem",
                  cursor: "pointer",
                  transition: "all 0.2s"
                }}
              >
                🏪 Marketplace
              </button>
            </div>

            {sidebarTab === "nodes" && (
              <NodePalette installedMarketplace={installedMarketplace} />
            )}

            {sidebarTab === "marketplace" && (
              <div className="marketplace-panel" style={{ padding: "0.75rem 0.9rem", display: "flex", flexDirection: "column", gap: "0.5rem" }}>
                <h2 style={{ fontSize: "0.75rem", textTransform: "uppercase", letterSpacing: "0.06em", margin: "0 0 0.2rem", color: "rgba(255,255,255,0.9)" }}>
                  Marketplace Catalog
                </h2>
                <p className="muted" style={{ fontSize: "0.72rem", margin: 0, lineHeight: 1.3 }}>
                  Install open-source cloud databases, pubsub streams, and networking solutions used by enterprise companies.
                </p>

                <div className="marketplace-catalog" style={{ display: "flex", flexDirection: "column", gap: "0.8rem", overflowY: "visible", marginTop: "0.5rem" }}>
                  {MARKETPLACE_CATALOG.map((item) => {
                    const isInstalled = installedMarketplace.includes(item.id);
                    return (
                      <div
                        key={item.id}
                        className="marketplace-card"
                        style={{
                          background: "rgba(255, 255, 255, 0.02)",
                          border: `1px solid ${isInstalled ? "rgba(16, 185, 129, 0.25)" : "rgba(255, 255, 255, 0.06)"}`,
                          borderRadius: "8px",
                          padding: "0.75rem",
                          display: "flex",
                          flexDirection: "column",
                          gap: "0.4rem",
                          transition: "all 0.2s ease"
                        }}
                      >
                        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: "6px" }}>
                          <span style={{ fontSize: "1.1rem" }}>{item.icon}</span>
                          <span style={{ fontWeight: 600, fontSize: "0.82rem", color: isInstalled ? "#34d399" : "#e2e8f0" }}>{item.name}</span>
                          <span
                            className="badge"
                            style={{
                              fontSize: "0.62rem",
                              padding: "1px 5px",
                              backgroundColor: isInstalled ? "rgba(16, 185, 129, 0.15)" : "rgba(255, 255, 255, 0.08)",
                              color: isInstalled ? "#34d399" : "rgba(255,255,255,0.5)"
                            }}
                          >
                            {isInstalled ? "Active" : "Crate"}
                          </span>
                        </div>
                        <p style={{ fontSize: "0.72rem", margin: 0, color: "rgba(255, 255, 255, 0.6)", lineHeight: 1.3 }}>
                          {item.desc}
                        </p>
                        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginTop: "0.2rem", fontSize: "0.65rem", color: "rgba(255,255,255,0.4)" }}>
                          <code>{item.crate}</code>
                          <button
                            type="button"
                            onClick={() => handleToggleInstall(item.id, isInstalled)}
                            style={{
                              padding: "0.25rem 0.6rem",
                              borderRadius: "4px",
                              border: "none",
                              fontSize: "0.68rem",
                              fontWeight: 600,
                              cursor: "pointer",
                              background: isInstalled ? "rgba(239, 68, 68, 0.15)" : "linear-gradient(135deg, #6366f1 0%, #4f46e5 100%)",
                              color: isInstalled ? "#ef4444" : "white",
                              transition: "all 0.15s"
                            }}
                          >
                            {isInstalled ? "Uninstall" : "Install"}
                          </button>
                        </div>
                      </div>
                    );
                  })}
                </div>
              </div>
            )}

            <div className="ai-chat-container">
              <div className="ai-chat-header">
                <div className="ai-chat-title-group">
                  <h3>AI Chat Assistant</h3>
                </div>
                <div className="ai-session-controls">
                  <button
                    type="button"
                    className={`btn-llm-settings ${showLlmSettings ? "active" : ""}`}
                    onClick={() => setShowLlmSettings(!showLlmSettings)}
                    title="Configure AI Agent & API Keys"
                    style={{
                      background: showLlmSettings ? "rgba(99, 102, 241, 0.25)" : "rgba(127, 127, 127, 0.08)",
                      color: showLlmSettings ? "white" : "rgba(255, 255, 255, 0.8)",
                      border: showLlmSettings ? "1px solid rgba(99, 102, 241, 0.5)" : "1px solid rgba(127, 127, 127, 0.2)",
                      fontSize: "0.7rem",
                      padding: "0.2rem 0.45rem",
                      borderRadius: "4px",
                      cursor: "pointer",
                      fontWeight: 600,
                      transition: "all 0.15s ease",
                    }}
                  >
                    ⚙️ Config
                  </button>
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

              {showLlmSettings && (
                <div className="ai-settings-card" style={{
                  background: "rgba(15, 15, 20, 0.8)",
                  backdropFilter: "blur(12px)",
                  border: "1px solid rgba(255, 255, 255, 0.08)",
                  borderRadius: "8px",
                  padding: "0.8rem",
                  margin: "0.4rem 0",
                  display: "flex",
                  flexDirection: "column",
                  gap: "0.6rem",
                  boxShadow: "0 8px 32px rgba(0,0,0,0.5)",
                }}>
                  <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                    <h4 style={{ fontSize: "0.75rem", textTransform: "uppercase", letterSpacing: "0.05em", color: "#818cf8", margin: 0 }}>⚙️ AI Agent Setup</h4>
                    <button
                      type="button"
                      onClick={() => setShowLlmSettings(false)}
                      style={{ background: "none", border: "none", color: "rgba(255,255,255,0.4)", cursor: "pointer", fontSize: "0.75rem" }}
                    >
                      ✕
                    </button>
                  </div>

                  <div style={{ display: "flex", flexDirection: "column", gap: "0.2rem" }}>
                    <label style={{ fontSize: "0.65rem", color: "rgba(255,255,255,0.6)", fontWeight: 600 }}>Preferred AI Agent</label>
                    <select
                      value={llmProvider}
                      onChange={(e) => setLlmProvider(e.target.value)}
                      style={{
                        background: "#09090b",
                        border: "1px solid rgba(255,255,255,0.15)",
                        borderRadius: "4px",
                        color: "white",
                        padding: "0.25rem 0.4rem",
                        fontSize: "0.75rem",
                      }}
                    >
                      <option value="claude_cli">Claude CLI (Local, No Key)</option>
                      <option value="anthropic">Claude API (Anthropic)</option>
                      <option value="open_ai">OpenAI (GPT-4o)</option>
                      <option value="deep_seek">DeepSeek (V3/R1)</option>
                      <option value="kimi">Kimi (Moonshot)</option>
                      <option value="codex">Custom OpenAI Codex</option>
                    </select>
                  </div>

                  <div style={{ display: "flex", flexDirection: "column", gap: "0.2rem" }}>
                    <label style={{ fontSize: "0.65rem", color: "rgba(255,255,255,0.6)", fontWeight: 600 }}>Model Name Override</label>
                    <input
                      type="text"
                      placeholder={`Default: ${DEFAULT_MODELS[llmProvider] || ""}`}
                      value={llmModel}
                      onChange={(e) => saveLlmCredentials(llmApiKey, e.target.value)}
                      style={{
                        background: "#09090b",
                        border: "1px solid rgba(255,255,255,0.15)",
                        borderRadius: "4px",
                        color: "white",
                        padding: "0.25rem 0.4rem",
                        fontSize: "0.75rem",
                      }}
                    />
                  </div>

                  {llmProvider !== "claude_cli" && (
                    <div style={{ display: "flex", flexDirection: "column", gap: "0.2rem" }}>
                      <label style={{ fontSize: "0.65rem", color: "rgba(255,255,255,0.6)", fontWeight: 600 }}>API Access Token (Client-Stored Only)</label>
                      <input
                        type="password"
                        placeholder="Enter your custom API Key..."
                        value={llmApiKey}
                        onChange={(e) => saveLlmCredentials(e.target.value, llmModel)}
                        style={{
                          background: "#09090b",
                          border: "1px solid rgba(255,255,255,0.15)",
                          borderRadius: "4px",
                          color: "white",
                          padding: "0.25rem 0.4rem",
                          fontSize: "0.75rem",
                        }}
                      />
                    </div>
                  )}

                  {llmProvider === "claude_cli" && (
                    <div style={{
                      fontSize: "0.65rem",
                      color: "rgba(129, 140, 248, 0.85)",
                      lineHeight: "1.3",
                      background: "rgba(99, 102, 241, 0.08)",
                      border: "1px solid rgba(99, 102, 241, 0.18)",
                      borderRadius: "4px",
                      padding: "0.4rem 0.6rem",
                    }}>
                      ⚡ <strong>Zero-Key Fallback:</strong> Generates designs locally via the pre-installed <code>claude</code> CLI tool. Make sure <code>claude</code> is configured in your system terminal's PATH.
                    </div>
                  )}
                </div>
              )}

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


          </aside>

          <main className="studio-canvas" style={{ position: "relative", display: "flex", flexDirection: "column" }} onPointerMove={onCanvasPointerMove}>
            <div style={{ flex: 1, position: "relative", minHeight: 0 }}>
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
                onNodeDragStop={onNodeDragStop}
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
            </div>

            {/* Premium Fixed Bottom Console Drawer */}
            <div className={`studio-console-panel ${isConsoleCollapsed ? "collapsed" : ""}`}>
              <div className="studio-console-header" onClick={() => setIsConsoleCollapsed(!isConsoleCollapsed)}>
                <div className="studio-console-title-group">
                  <span className="studio-console-toggle-icon" style={{ fontSize: "0.7rem", color: "#818cf8" }}>
                    {isConsoleCollapsed ? "▲" : "▼"}
                  </span>
                  <span className="studio-console-title">Console Output Logs</span>
                  <div style={{ display: "flex", gap: "0.5rem", alignItems: "center" }}>
                    <BuildBadge state={buildState} />
                    {isRunning && <span className="badge badge-ok">running</span>}
                  </div>
                </div>
                <div className="studio-console-actions" onClick={(e) => e.stopPropagation()}>
                  <button
                    type="button"
                    className="btn-console-action"
                    onClick={() => setBuildLines([])}
                    title="Clear console logs"
                  >
                    🗑️ Clear Logs
                  </button>
                  <button
                    type="button"
                    className="btn-console-action"
                    onClick={() => setIsConsoleCollapsed(!isConsoleCollapsed)}
                  >
                    {isConsoleCollapsed ? "Expand" : "Collapse"}
                  </button>
                </div>
              </div>
              
              {!isConsoleCollapsed && (
                <div className="studio-console-body">
                  {buildLines.length === 0 ? (
                    <div style={{ opacity: 0.4, fontStyle: "italic", fontSize: "0.75rem", padding: "0.5rem 0" }}>
                      No logs. Click "Check", "Run", "Debug", or "Test" to execute.
                    </div>
                  ) : (
                    buildLines.map((line, i) => <pre key={i}>{line}</pre>)
                  )}
                  <div ref={linesEndRef} />
                </div>
              )}
            </div>
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
              onDelete={() => deleteNode(selectedNode.id)}
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
