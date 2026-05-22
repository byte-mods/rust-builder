import { memo } from "react";
import { Handle, Position, type Node, type NodeProps } from "@xyflow/react";

interface EntryPointData extends Record<string, unknown> {
  label: string;
  templateId: string;
  config: unknown;
}

type EntryPointNodeType = Node<EntryPointData, "entryPoint">;

/// Custom ReactFlow node for the application entry point (core.entry_point).
/// Renders larger and more prominently than default nodes to signal that
/// this is main.rs — the root from which the entire flow begins.
const EntryPointNode = memo(function EntryPointNode({
  data,
  selected,
}: NodeProps<EntryPointNodeType>) {
  return (
    <div className={`entry-point-node ${selected ? "selected" : ""}`}>
      <div className="entry-point-header">
        <span className="entry-point-icon">⚡</span>
        <span className="entry-point-label">{(data as EntryPointData).label}</span>
      </div>
      <div className="entry-point-subtitle">Application Entry Point</div>

      {/* Output handles on the right side */}
      <Handle
        type="source"
        position={Position.Right}
        id="http"
        style={{ top: "25%" }}
      />
      <Handle
        type="source"
        position={Position.Right}
        id="consumer"
        style={{ top: "50%" }}
      />
      <Handle
        type="source"
        position={Position.Right}
        id="scheduler"
        style={{ top: "75%" }}
      />
      <Handle
        type="source"
        position={Position.Bottom}
        id="service"
      />
    </div>
  );
});

export default EntryPointNode;
