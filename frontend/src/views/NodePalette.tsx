import { useEffect, useMemo, useState } from "react";
import { ApiError, fetchTemplates, type Template } from "../api";

type PaletteState =
  | { kind: "loading" }
  | { kind: "ready"; templates: Template[] }
  | { kind: "error"; message: string };

/// Side panel listing every registered node template grouped by category.
/// Items are draggable onto the ReactFlow canvas (S8).
export default function NodePalette(): JSX.Element {
  const [state, setState] = useState<PaletteState>({ kind: "loading" });

  useEffect(() => {
    const controller = new AbortController();
    fetchTemplates(controller.signal)
      .then((templates) => setState({ kind: "ready", templates }))
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === "AbortError") return;
        const message = err instanceof ApiError ? err.message : "unknown error";
        setState({ kind: "error", message });
      });
    return () => controller.abort();
  }, []);

  const grouped = useMemo(() => {
    if (state.kind !== "ready") return [];
    const buckets = new Map<string, Template[]>();
    for (const t of state.templates) {
      const bucket = buckets.get(t.display.category) ?? [];
      bucket.push(t);
      buckets.set(t.display.category, bucket);
    }
    return Array.from(buckets.entries()).sort(([a], [b]) => a.localeCompare(b));
  }, [state]);

  function onDragStart(event: React.DragEvent<HTMLLIElement>, template: Template) {
    event.dataTransfer.setData("application/reactflow", JSON.stringify({
      templateId: template.id,
      // Provide a sensible default config so the node is immediately valid.
      defaultConfig: buildDefaultConfig(template.config_schema),
    }));
    event.dataTransfer.effectAllowed = "move";
  }

  return (
    <aside className="node-palette">
      <h2>Nodes</h2>
      {state.kind === "loading" && <p className="muted">loading…</p>}
      {state.kind === "error" && (
        <p className="form-error">templates unavailable: {state.message}</p>
      )}
      {state.kind === "ready" && grouped.length === 0 && (
        <p className="muted">no templates registered</p>
      )}
      {state.kind === "ready" &&
        grouped.map(([category, items]) => (
          <section key={category} className="palette-group">
            <h3>{category}</h3>
            <ul>
              {items.map((t) => (
                <li
                  key={t.id}
                  className="palette-item"
                  title={t.display.description}
                  draggable
                  onDragStart={(e) => onDragStart(e, t)}
                >
                  <span className="palette-item-name">{t.display.name}</span>
                  <code className="palette-item-id">{t.id}</code>
                </li>
              ))}
            </ul>
          </section>
        ))}
    </aside>
  );
}

/// Build a minimal valid default config from a JSON Schema object.
/// This is best-effort; unsupported schemas fall back to `{}`.
function buildDefaultConfig(schema: unknown): unknown {
  if (typeof schema !== "object" || schema === null) return {};
  const s = schema as Record<string, unknown>;
  if (s.type !== "object") return {};
  const props = s.properties as Record<string, unknown> | undefined;
  if (!props) return {};

  const out: Record<string, unknown> = {};
  for (const [key, propSchema] of Object.entries(props)) {
    out[key] = defaultForProperty(propSchema);
  }
  return out;
}

function defaultForProperty(propSchema: unknown): unknown {
  if (typeof propSchema !== "object" || propSchema === null) return "";
  const s = propSchema as Record<string, unknown>;

  if (Array.isArray(s.enum) && s.enum.length > 0) {
    return s.enum[0];
  }

  switch (s.type) {
    case "string":
      return "";
    case "integer":
    case "number":
      return 0;
    case "boolean":
      return false;
    case "array":
      return [];
    case "object": {
      const nested = s.properties as Record<string, unknown> | undefined;
      if (!nested) return {};
      const obj: Record<string, unknown> = {};
      for (const [k, v] of Object.entries(nested)) {
        obj[k] = defaultForProperty(v);
      }
      return obj;
    }
    default:
      return "";
  }
}
