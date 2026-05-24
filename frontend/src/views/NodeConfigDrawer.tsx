import { useMemo } from "react";
import { type Template, type ParsedDiagnostic } from "../api";
import SchemaForm from "./SchemaForm";

interface NodeConfigDrawerProps {
  slug: string;
  nodeId: string;
  templateId: string;
  config: unknown;
  comment?: string;
  onCommentChange: (comment: string) => void;
  templates: Template[];
  diagnostics: ParsedDiagnostic[];
  onChange: (config: unknown) => void;
  onClose: () => void;
}

/// Side panel that opens when a node is selected on the canvas.
/// Renders a form generated from the template's JSON Schema.
export default function NodeConfigDrawer({
  slug,
  nodeId,
  templateId,
  config,
  comment = "",
  onCommentChange,
  templates,
  diagnostics,
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
          <SchemaForm
            schema={template.config_schema}
            value={config}
            diagnostics={diagnostics}
            onChange={onChange}
            slug={slug}
          />
        ) : (
          <p className="muted">template not found</p>
        )}

        <div className="drawer-comment-section" style={{ marginTop: "1.25rem", borderTop: "1px solid rgba(127, 127, 127, 0.18)", paddingTop: "0.8rem" }}>
          <label style={{ fontSize: "0.72rem", textTransform: "uppercase", letterSpacing: "0.06em", color: "rgba(127, 127, 127, 0.9)", display: "block", marginBottom: "0.4rem", fontWeight: 600 }}>
            Node Note / Comment
          </label>
          <textarea
            style={{
              width: "100%",
              boxSizing: "border-box",
              padding: "0.45rem 0.6rem",
              borderRadius: "6px",
              border: "1px solid rgba(127, 127, 127, 0.25)",
              background: "rgba(0, 0, 0, 0.25)",
              color: "white",
              fontFamily: "inherit",
              fontSize: "0.8rem",
              resize: "vertical",
              lineHeight: "1.4",
              outline: "none"
            }}
            placeholder="Describe what this node does or note down config details..."
            value={comment}
            onChange={(e) => onCommentChange(e.target.value)}
            rows={3}
          />
        </div>
      </div>
    </aside>
  );
}

