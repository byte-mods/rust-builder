import { useCallback } from "react";
import MonacoEditor, { type EditorMarker } from "../components/MonacoEditor";
import { type ParsedDiagnostic } from "../api";

const CODE_FIELDS: Record<string, { height: string; language: string }> = {
  body: { height: "220px", language: "rust" },
  condition: { height: "90px", language: "rust" },
  true_expr: { height: "110px", language: "rust" },
  false_expr: { height: "110px", language: "rust" },
  expr: { height: "110px", language: "rust" },
  where_clause: { height: "100px", language: "rust" },
  code: { height: "380px", language: "rust" },
};

interface SchemaFormProps {
  schema: unknown;
  value: unknown;
  diagnostics?: ParsedDiagnostic[];
  onChange: (value: unknown) => void;
}

/// Best-effort dynamic form renderer from a JSON Schema object.
/// Handles the shapes produced by schemars for the seven built-in templates.
export default function SchemaForm({
  schema,
  value,
  diagnostics = [],
  onChange,
}: SchemaFormProps): JSX.Element {
  const objValue = (typeof value === "object" && value !== null ? value : {}) as Record<string, unknown>;

  const setField = useCallback(
    (key: string, fieldValue: unknown) => {
      onChange({ ...objValue, [key]: fieldValue });
    },
    [objValue, onChange]
  );

  if (typeof schema !== "object" || schema === null) {
    return <p className="muted">no schema available</p>;
  }

  const s = schema as Record<string, unknown>;
  if (s.type !== "object") {
    return <p className="muted">unsupported schema type: {String(s.type)}</p>;
  }

  const properties = (s.properties ?? {}) as Record<string, unknown>;
  const required = new Set(Array.isArray(s.required) ? (s.required as string[]) : []);

  return (
    <div className="schema-form">
      {Object.entries(properties)
        .filter(([key]) => key !== "inputs" && key !== "outputs")
        .map(([key, propSchema]) => (
          <SchemaField
            key={key}
            name={key}
            schema={propSchema}
            value={objValue[key]}
            required={required.has(key)}
            diagnostics={diagnostics}
            onChange={(v) => setField(key, v)}
          />
        ))}
    </div>
  );
}

interface SchemaFieldProps {
  name: string;
  schema: unknown;
  value: unknown;
  required: boolean;
  diagnostics?: ParsedDiagnostic[];
  onChange: (value: unknown) => void;
}

function SchemaField({
  name,
  schema,
  value,
  required,
  diagnostics = [],
  onChange,
}: SchemaFieldProps): JSX.Element {
  if (typeof schema !== "object" || schema === null) {
    return (
      <label>
        {name}
        <input type="text" value={String(value ?? "")} onChange={(e) => onChange(e.target.value)} />
      </label>
    );
  }

  const s = schema as Record<string, unknown>;

  // Enum → select
  if (Array.isArray(s.enum) && s.enum.length > 0) {
    const options = s.enum as (string | number | boolean)[];
    return (
      <label>
        {name}{required && <span className="required">*</span>}
        <select value={String(value ?? options[0])} onChange={(e) => onChange(e.target.value)}>
          {options.map((opt) => (
            <option key={String(opt)} value={String(opt)}>
              {String(opt)}
            </option>
          ))}
        </select>
      </label>
    );
  }

  switch (s.type) {
    case "boolean":
      return (
        <label className="field-inline">
          <input
            type="checkbox"
            checked={Boolean(value)}
            onChange={(e) => onChange(e.target.checked)}
          />
          {name}
        </label>
      );

    case "array": {
      const items = s.items as Record<string, unknown> | undefined;
      const arr = Array.isArray(value) ? (value as unknown[]) : [];
      return (
        <ArrayField
          name={name}
          itemSchema={items}
          value={arr}
          onChange={onChange}
        />
      );
    }

    case "integer":
    case "number":
      return (
        <label>
          {name}{required && <span className="required">*</span>}
          <input
            type="number"
            value={typeof value === "number" ? value : 0}
            onChange={(e) => onChange(Number(e.target.value))}
          />
        </label>
      );

    case "string":
    default: {
      if (name in CODE_FIELDS) {
        const spec = CODE_FIELDS[name];

        // Map compiler line errors to Monaco Editor markers with line-offset rules
        const markers: EditorMarker[] = diagnostics.map((d) => {
          const startLine = name === "body" ? Math.max(1, d.line - 2) : 1;
          return {
            startLineNumber: startLine,
            startColumn: d.column,
            endLineNumber: startLine,
            endColumn: d.column + 5,
            message: d.message,
            severity: d.severity === "warning" ? "warning" : "error",
          };
        });

        return (
          <label>
            {name}{required && <span className="required">*</span>}
            <MonacoEditor
              value={typeof value === "string" ? value : ""}
              onChange={onChange}
              height={spec.height}
              language={spec.language}
              markers={markers}
            />
          </label>
        );
      }
      return (
        <label>
          {name}{required && <span className="required">*</span>}
          <input
            type="text"
            value={typeof value === "string" ? value : ""}
            onChange={(e) => onChange(e.target.value)}
          />
        </label>
      );
    }
  }
}

interface ArrayFieldProps {
  name: string;
  itemSchema: Record<string, unknown> | undefined;
  value: unknown[];
  onChange: (value: unknown) => void;
}

function ArrayField({ name, itemSchema, value, onChange }: ArrayFieldProps): JSX.Element {
  function addItem() {
    const defaultItem = buildDefaultConfig(itemSchema);
    onChange([...value, defaultItem]);
  }

  function removeItem(index: number) {
    const copy = [...value];
    copy.splice(index, 1);
    onChange(copy);
  }

  function updateItem(index: number, itemValue: unknown) {
    const copy = [...value];
    copy[index] = itemValue;
    onChange(copy);
  }

  return (
    <div className="array-field">
      <div className="array-header">
        <strong>{name}</strong>
        <button type="button" onClick={addItem}>+ add</button>
      </div>
      {value.length === 0 && <p className="muted">no items</p>}
      {value.map((item, i) => (
        <div key={i} className="array-item">
          <div className="array-item-header">
            <span>#{i + 1}</span>
            <button type="button" className="danger" onClick={() => removeItem(i)}>
              remove
            </button>
          </div>
          {itemSchema?.type === "object" ? (
            <SchemaForm schema={itemSchema} value={item} onChange={(v) => updateItem(i, v)} />
          ) : (
            <SchemaField
              name="value"
              schema={itemSchema}
              value={item}
              required={false}
              onChange={(v) => updateItem(i, v)}
            />
          )}
        </div>
      ))}
    </div>
  );
}

function buildDefaultConfig(schema: unknown): unknown {
  if (typeof schema !== "object" || schema === null) return {};
  const s = schema as Record<string, unknown>;
  if (s.type !== "object") {
    switch (s.type) {
      case "string": return "";
      case "integer":
      case "number": return 0;
      case "boolean": return false;
      case "array": return [];
      default: return "";
    }
  }
  const props = s.properties as Record<string, unknown> | undefined;
  if (!props) return {};
  const out: Record<string, unknown> = {};
  for (const [key, propSchema] of Object.entries(props)) {
    out[key] = buildDefaultConfig(propSchema);
  }
  return out;
}
