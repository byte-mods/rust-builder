import { memo } from "react";
import { Handle, Position, type Node, type NodeProps } from "@xyflow/react";

export interface PortInfo {
  name: string;
  type_tag: string;
  description?: string;
}

export interface NodeDiagnostic {
  severity: "error" | "warning";
  message: string;
  code?: string;
  line?: number;
  column?: number;
}

interface StudioNodeData extends Record<string, unknown> {
  label: string;
  templateId: string;
  config: Record<string, unknown>;
  inputs: PortInfo[];
  outputs: PortInfo[];
  diagnostics?: NodeDiagnostic[];
  isBreakpoint?: boolean;
  isPaused?: boolean;
  onToggleBreakpoint?: (nodeId: string) => void;
  metrics?: { throughput: number; avg_latency_us: number; p99_latency_us: number };
  comment?: string;
}

type StudioNodeType = Node<StudioNodeData, "studio">;

const CATEGORY_COLORS: Record<string, { bg: string; icon: string }> = {
  "language.struct": { bg: "linear-gradient(135deg, #6366f1 0%, #4f46e5 100%)", icon: "📦" },
  "language.enum": { bg: "linear-gradient(135deg, #818cf8 0%, #6366f1 100%)", icon: "🗂️" },
  "language.fn": { bg: "linear-gradient(135deg, #4f46e5 0%, #3730a3 100%)", icon: "⚙️" },
  "language.if_else": { bg: "linear-gradient(135deg, #4338ca 0%, #312e81 100%)", icon: "🌿" },
  "language.match": { bg: "linear-gradient(135deg, #312e81 0%, #1e1b4b 100%)", icon: "🎯" },
  "language.loop": { bg: "linear-gradient(135deg, #3730a3 0%, #1e1b4b 100%)", icon: "🔁" },
  "http.route": { bg: "linear-gradient(135deg, #0d9488 0%, #0f766e 100%)", icon: "🌐" },
  "http.handler": { bg: "linear-gradient(135deg, #14b8a6 0%, #0d9488 100%)", icon: "📬" },
  "core.service": { bg: "linear-gradient(135deg, #10b981 0%, #047a3e 100%)", icon: "💎" },
  "core.dto": { bg: "linear-gradient(135deg, #3b82f6 0%, #1d4ed8 100%)", icon: "📄" },
  "observability.logger": { bg: "linear-gradient(135deg, #f43f5e 0%, #be123c 100%)", icon: "🪵" },
  "parser.json": { bg: "linear-gradient(135deg, #f97316 0%, #ea580c 100%)", icon: "{}" },
  "parser.xml": { bg: "linear-gradient(135deg, #fdba74 0%, #f97316 100%)", icon: "📝" },
  "parser.protobuf": { bg: "linear-gradient(135deg, #ea580c 0%, #c2410c 100%)", icon: "🧬" },
  "integration.consumer": { bg: "linear-gradient(135deg, #d97706 0%, #b45309 100%)", icon: "📥" },
  "integration.scheduler": { bg: "linear-gradient(135deg, #eab308 0%, #ca8a04 100%)", icon: "⏰" },
};

function getTemplateCategory(templateId: string): { bg: string; icon: string; name: string } {
  // Try exact match
  if (CATEGORY_COLORS[templateId]) {
    const label = templateId.split(".").pop() || templateId;
    return {
      bg: CATEGORY_COLORS[templateId].bg,
      icon: CATEGORY_COLORS[templateId].icon,
      name: label.charAt(0).toUpperCase() + label.slice(1),
    };
  }

  // Prefix match
  for (const [prefix, val] of Object.entries(CATEGORY_COLORS)) {
    if (templateId.startsWith(prefix)) {
      const label = templateId.split(".").pop() || templateId;
      return {
        bg: val.bg,
        icon: val.icon,
        name: label.charAt(0).toUpperCase() + label.slice(1),
      };
    }
  }

  // Fallbacks
  if (templateId.startsWith("tokio.")) {
    const label = templateId.split(".").pop() || templateId;
    return {
      bg: "linear-gradient(135deg, #d97706 0%, #ca8a04 100%)",
      icon: "⚡",
      name: label.toUpperCase(),
    };
  }

  const label = templateId.split(".").pop() || templateId;
  return {
    bg: "linear-gradient(135deg, #64748b 0%, #475569 100%)",
    icon: "🧩",
    name: label.charAt(0).toUpperCase() + label.slice(1),
  };
}

const StudioNode = memo(function StudioNode({
  id,
  data,
  selected,
}: NodeProps<StudioNodeType>) {
  const {
    label,
    templateId,
    inputs = [],
    outputs = [],
    diagnostics = [],
    isBreakpoint = false,
    isPaused = false,
    onToggleBreakpoint,
    metrics,
    comment,
  } = data;

  const category = getTemplateCategory(templateId);

  const errors = diagnostics.filter((d) => d.severity === "error");
  const warnings = diagnostics.filter((d) => d.severity === "warning");

  const hasErrors = errors.length > 0;
  const hasWarnings = warnings.length > 0 && !hasErrors;

  const stateClass = [
    hasErrors ? "has-error" : hasWarnings ? "has-warning" : "",
    isPaused ? "is-paused" : "",
    isBreakpoint ? "has-breakpoint" : ""
  ].filter(Boolean).join(" ");

  return (
    <div className={`studio-node ${selected ? "selected" : ""} ${stateClass}`}>
      {/* Node Header */}
      <header className="node-header" style={{ background: category.bg }}>
        {onToggleBreakpoint && (
          <button
            type="button"
            className={`node-breakpoint-toggle ${isBreakpoint ? "active" : ""}`}
            onClick={(e) => {
              e.stopPropagation();
              onToggleBreakpoint(id);
            }}
            title={isBreakpoint ? "Remove Breakpoint" : "Add Breakpoint"}
          >
            ●
          </button>
        )}
        <span className="node-header-icon">{category.icon}</span>
        <div className="node-header-title">
          <span className="node-title-text">{label}</span>
          <span className="node-category-tag">{category.name}</span>
        </div>
      </header>

      {/* Node Body with Labeled Sockets */}
      <div className="node-body">
        {/* Left Side: Inputs */}
        <div className="node-ports-column inputs-column">
          {inputs.map((input, idx) => {
            const top = `${((idx + 1) * 100) / (inputs.length + 1)}%`;
            return (
              <div key={input.name} className="node-port-item input-item" style={{ top }}>
                <Handle
                  type="target"
                  position={Position.Left}
                  id={input.name}
                  className="port-handle input-handle"
                />
                <span className="port-label" title={input.description || input.name}>
                  {input.name} <span className="port-type">{input.type_tag}</span>
                </span>
              </div>
            );
          })}
        </div>

        {/* Placeholder if no ports are defined to maintain styling */}
        {inputs.length === 0 && outputs.length === 0 && (
          <div className="node-empty-content">Static Config Block</div>
        )}

        {/* Right Side: Outputs */}
        <div className="node-ports-column outputs-column">
          {outputs.map((output, idx) => {
            const top = `${((idx + 1) * 100) / (outputs.length + 1)}%`;
            return (
              <div key={output.name} className="node-port-item output-item" style={{ top }}>
                <span className="port-label" title={output.description || output.name}>
                  <span className="port-type">{output.type_tag}</span> {output.name}
                </span>
                <Handle
                  type="source"
                  position={Position.Right}
                  id={output.name}
                  className="port-handle output-handle"
                />
              </div>
            );
          })}
        </div>
      </div>

      {/* Floating Diagnostic Badges + Tooltips */}
      {diagnostics.length > 0 && (
        <div className={`node-diagnostic-badge ${hasErrors ? "error" : "warning"}`}>
          {diagnostics.length}
          
          <div className="node-diagnostic-tooltip">
            <h4 className="tooltip-title">
              {hasErrors ? "❌ Compiler Errors" : "⚠️ Compiler Warnings"} ({diagnostics.length})
            </h4>
            <ul className="tooltip-list">
              {diagnostics.map((diag, i) => (
                <li key={i} className={`tooltip-item ${diag.severity}`}>
                  <span className="tooltip-code">{diag.code || "rustc"}:</span>
                  <span className="tooltip-msg">{diag.message}</span>
                  {diag.line && diag.column && (
                    <span className="tooltip-loc"> [L{diag.line}:C{diag.column}]</span>
                  )}
                </li>
              ))}
            </ul>
          </div>
        </div>
      )}

      {/* Floating Comment Badge + Tooltip */}
      {comment && comment.trim() && (
        <div className="node-comment-indicator">
          💬
          <div className="node-comment-tooltip">
            <div style={{ fontWeight: 600, borderBottom: "1px solid rgba(255, 255, 255, 0.1)", paddingBottom: "0.2rem", marginBottom: "0.3rem", color: "#60a5fa" }}>
              Note
            </div>
            {comment}
          </div>
        </div>
      )}

      {/* S21: Performance Profiler Pill */}
      {metrics && metrics.throughput > 0 && (
        <div className="node-performance-pill">
          <span className="perf-metric throughput" title="Throughput (Events per second)">
            ⚡ {metrics.throughput}/s
          </span>
          <span className="perf-separator">·</span>
          <span className="perf-metric latency" title="99th Percentile Execution Latency">
            ⏱ {metrics.p99_latency_us >= 1000 
              ? `${(metrics.p99_latency_us / 1000).toFixed(1)}ms` 
              : `${metrics.p99_latency_us}µs`}
          </span>
        </div>
      )}
    </div>
  );
});

export default StudioNode;
