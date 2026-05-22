import { useMemo } from "react";
import { type Template } from "../api";
import SchemaForm from "./SchemaForm";

interface NodeConfigDrawerProps {
  nodeId: string;
  templateId: string;
  config: unknown;
  templates: Template[];
  onChange: (config: unknown) => void;
  onClose: () => void;
}

/// Side panel that opens when a node is selected on the canvas.
/// Renders a form generated from the template's JSON Schema.
export default function NodeConfigDrawer({
  nodeId,
  templateId,
  config,
  templates,
  onChange,
  onClose,
}: NodeConfigDrawerProps): JSX.Element {
  const template = useMemo(
    () => templates.find((t) => t.id === templateId),
    [templates, templateId]
  );

  return (
    <aside className="node-config-drawer">
      <header className="drawer-header">
        <h3>Node config</h3>
        <button type="button" onClick={onClose} aria-label="Close">
          ✕
        </button>
      </header>

      <div className="drawer-body">
        <div className="drawer-meta">
          <p>
            <span className="muted">ID:</span> <code>{nodeId}</code>
          </p>
          <p>
            <span className="muted">Template:</span>{" "}
            {template ? template.display.name : templateId}
          </p>
        </div>

        {template ? (
          <SchemaForm schema={template.config_schema} value={config} onChange={onChange} />
        ) : (
          <p className="muted">template not found</p>
        )}
      </div>
    </aside>
  );
}
