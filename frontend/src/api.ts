// Studio backend API client.
//
// This module is the single source of truth for the wire contract between
// the studio frontend and the Axum backend. Every type here mirrors the
// serde-derived shape in `backend/src/projects/types.rs` (camelCase aside —
// the backend uses snake_case for one field, `schema_version`; the wire
// keeps that name).
//
// Error model: every non-2xx response from the backend is expected to carry
// a JSON envelope `{error: string, message: string}` (defined by
// `backend/src/error.rs::ApiErrorBody`). Network failures and unexpected
// shapes are mapped onto an `ApiError` with `code = "network"` /
// `code = "malformed_response"` so call sites can `switch (err.code)`
// uniformly.

/// Backend base URL. Resolved at build time from `VITE_API_BASE`. An empty
/// string is treated as "unset" (FOLLOWUP-S1-B close) — without this guard
/// an explicitly-empty env var would silently produce relative URLs and hit
/// the Vite dev server instead of the studio backend.
const RAW_API_BASE = (import.meta.env.VITE_API_BASE ?? "") as string;
export const API_BASE: string = RAW_API_BASE.length > 0 ? RAW_API_BASE : "http://127.0.0.1:7878";

// ---- Domain types (mirror backend serde shapes) ----------------------------

export interface HealthResponse {
  status: string;
  version: string;
}

export interface ProjectMeta {
  slug: string;
  name: string;
  /** RFC 3339 timestamp produced by `time::OffsetDateTime`. */
  created_at: string;
  /** RFC 3339 timestamp; advances on every `save_graph`. */
  updated_at: string;
  schema_version: number;
}

export interface Project extends ProjectMeta {}

export type NodeKind =
  | "route"
  | "handler"
  | "service"
  | "dto"
  | "consumer"
  | "scheduler"
  | "logger";

export interface Position {
  x: number;
  y: number;
}

export interface GraphNode {
  id: string;
  /** S3+ canonical field. The backend deserializer accepts both `template_id`
   *  and the legacy `kind` field, but always serialises `template_id`. */
  template_id?: string;
  /** S2 legacy field — present only in graphs that have not been saved since S3. */
  kind?: NodeKind;
  position: Position;
  config: unknown;
  label?: string;
  comment?: string;
}

export interface GraphEdge {
  id: string;
  source: string;
  target: string;
  source_port: string;
  target_port: string;
}

export interface Graph {
  schema_version: number;
  nodes: GraphNode[];
  edges: GraphEdge[];
}

/// Empty graph at the current schema version — useful as the initial state
/// for a brand-new project view before the first PUT lands.
export const EMPTY_GRAPH: Graph = { schema_version: 1, nodes: [], edges: [] };

// ---- Templates (Section 3) -------------------------------------------------

/// Cardinality of an edge attached to a template port. Mirrors the
/// backend's `PortMultiplicity`.
export type PortMultiplicity = "single" | "optional" | "many";

/// Wire shape of `PortSpec` from `backend/src/templates/ports.rs`.
export interface TemplatePort {
  name: string;
  type_tag: string;
  multiplicity: PortMultiplicity;
  doc: string;
}

/// Codegen modes a template can declare. `runtime` is the default; `codegen`
/// is for schema-driven type generation (DTOs, Section 9's parser pack in
/// codegen mode); `both` is reserved for Section 9's parser pack at the
/// instance level.
export type CodegenMode = "runtime" | "codegen" | "both";

/// Debug-bridge instrumentation contract. Section 13's step debugger keys
/// per-instance behaviour off this.
export type DebugBridgeKind = "default" | "long_runner" | "stream" | "pass_through";

export interface TemplateDisplay {
  name: string;
  category: string;
  description: string;
}

/// Wire shape of `TemplateSummary` from `backend/src/templates/mod.rs`.
export interface Template {
  id: string;
  display: TemplateDisplay;
  input_ports: TemplatePort[];
  output_ports: TemplatePort[];
  codegen_mode: CodegenMode;
  debug_bridge: DebugBridgeKind;
  /** JSON Schema (as JSON Value) describing per-instance config. */
  config_schema: unknown;
}

/// `GET /api/templates` — sorted by id.
export async function fetchTemplates(signal?: AbortSignal): Promise<Template[]> {
  const body = await request<{ templates: Template[] }>(
    "GET",
    "/api/templates",
    { signal },
  );
  return body.templates;
}

// ---- Error model -----------------------------------------------------------

/// Stable wire-shape of the JSON body the backend returns on any 4xx/5xx.
interface ApiErrorBody {
  error: string;
  message: string;
}

/// Typed error class used for every failure mode this module surfaces:
/// - HTTP error response with a parseable `ApiErrorBody`.
/// - HTTP error response with a non-JSON body (`code = "malformed_response"`).
/// - Network failure (`code = "network"`).
///
/// Call sites switch on `err.code` (one of the backend's snake_case codes,
/// plus the two client-side synthetics above) and surface `err.message`
/// directly to the UI — backend messages are already sanitised, and the
/// synthetic codes carry a generic safe message.
export class ApiError extends Error {
  readonly code: string;
  readonly status: number | null;

  constructor(code: string, message: string, status: number | null) {
    super(message);
    this.code = code;
    this.status = status;
    // Restore prototype chain for `instanceof ApiError` across the TS/JS
    // class transpilation boundary — without this, ES5-target builds break.
    Object.setPrototypeOf(this, ApiError.prototype);
  }
}

// ---- Internal request helpers ---------------------------------------------

/// Type guard for `ApiErrorBody`. Used after parsing JSON returned by an
/// HTTP error response — any missing/wrong field demotes the result to the
/// `malformed_response` synthetic.
function isApiErrorBody(value: unknown): value is ApiErrorBody {
  if (typeof value !== "object" || value === null) return false;
  const v = value as Record<string, unknown>;
  return typeof v.error === "string" && typeof v.message === "string";
}

/// Centralised fetch wrapper. Issues the request, threads `AbortSignal`,
/// distinguishes 2xx / 4xx-5xx / network failure, parses the appropriate
/// body, and normalises everything into either a typed success value or a
/// thrown `ApiError`.
async function request<T>(
  method: string,
  path: string,
  init?: { body?: unknown; signal?: AbortSignal; expectEmptyBody?: boolean },
): Promise<T> {
  const url = `${API_BASE}${path}`;
  let res: Response;
  try {
    res = await fetch(url, {
      method,
      signal: init?.signal,
      headers: init?.body !== undefined ? { "content-type": "application/json" } : undefined,
      body: init?.body !== undefined ? JSON.stringify(init.body) : undefined,
    });
  } catch (cause: unknown) {
    if (cause instanceof DOMException && cause.name === "AbortError") {
      throw cause; // propagate aborts unchanged so callers can swallow them
    }
    throw new ApiError("network", "studio backend is unreachable", null);
  }

  if (!res.ok) {
    let parsed: unknown = null;
    try {
      parsed = await res.json();
    } catch {
      // Body wasn't JSON — fall through to malformed_response.
    }
    if (isApiErrorBody(parsed)) {
      throw new ApiError(parsed.error, parsed.message, res.status);
    }
    throw new ApiError(
      "malformed_response",
      `backend returned HTTP ${res.status} with a body the client could not parse`,
      res.status,
    );
  }

  if (init?.expectEmptyBody) {
    return undefined as T;
  }

  const raw: unknown = await res.json();
  return raw as T;
}

// ---- Public API ------------------------------------------------------------

/// Probe `/health` and return the parsed body. Validates the response shape
/// at runtime (FOLLOWUP-S1-B close — previously `as HealthResponse` cast
/// preceded the validation; now the validation owns the cast through an
/// `unknown` intermediate).
export async function fetchHealth(signal?: AbortSignal): Promise<HealthResponse> {
  const body = await request<unknown>("GET", "/health", { signal });
  if (
    typeof body !== "object" ||
    body === null ||
    typeof (body as Record<string, unknown>).status !== "string" ||
    typeof (body as Record<string, unknown>).version !== "string"
  ) {
    throw new ApiError(
      "malformed_response",
      "backend /health returned a body that does not match HealthResponse",
      200,
    );
  }
  return body as HealthResponse;
}

/// `GET /api/projects` — newest first.
export async function listProjects(signal?: AbortSignal): Promise<ProjectMeta[]> {
  const body = await request<{ projects: ProjectMeta[] }>(
    "GET",
    "/api/projects",
    { signal },
  );
  return body.projects;
}

/// `POST /api/projects` — create a new project. Throws `ApiError("invalid_body", ...)`
/// if the slug fails server-side validation; the UI should pre-validate
/// where possible to give faster feedback.
export async function createProject(
  slug: string,
  name: string,
  signal?: AbortSignal,
): Promise<Project> {
  return request<Project>("POST", "/api/projects", {
    body: { slug, name },
    signal,
  });
}

/// `GET /api/projects/:slug`.
export async function getProject(slug: string, signal?: AbortSignal): Promise<Project> {
  return request<Project>("GET", `/api/projects/${encodeURIComponent(slug)}`, { signal });
}

/// `DELETE /api/projects/:slug`. Resolves with no value on 204.
export async function deleteProject(slug: string, signal?: AbortSignal): Promise<void> {
  await request<void>("DELETE", `/api/projects/${encodeURIComponent(slug)}`, {
    signal,
    expectEmptyBody: true,
  });
}

/// `POST /api/projects/import` — uploads a binary .flow package to import it.
export async function importProject(
  fileBytes: ArrayBuffer,
  signal?: AbortSignal,
): Promise<Project> {
  const url = `${API_BASE}/api/projects/import`;
  let res: Response;
  try {
    res = await fetch(url, {
      method: "POST",
      headers: {
        "content-type": "application/octet-stream",
      },
      body: fileBytes,
      signal,
    });
  } catch (cause: unknown) {
    if (cause instanceof DOMException && cause.name === "AbortError") {
      throw cause;
    }
    throw new ApiError("network", "studio backend is unreachable", null);
  }

  if (!res.ok) {
    let parsed: unknown = null;
    try {
      parsed = await res.json();
    } catch {
      // Body wasn't JSON
    }
    if (parsed && typeof parsed === "object" && "error" in parsed && "message" in parsed) {
      const err = parsed as { error: string; message: string };
      throw new ApiError(err.error, err.message, res.status);
    }
    throw new ApiError(
      "malformed_response",
      `backend returned HTTP ${res.status} with a body the client could not parse`,
      res.status,
    );
  }

  return res.json() as Promise<Project>;
}

/// `GET /api/projects/:slug/graph`.
export async function loadGraph(slug: string, signal?: AbortSignal): Promise<Graph> {
  return request<Graph>("GET", `/api/projects/${encodeURIComponent(slug)}/graph`, { signal });
}

/// `PUT /api/projects/:slug/graph` — replace the persisted graph atomically.
/// Returns the persisted value as confirmation (echoes the body the backend
/// wrote to disk).
export async function saveGraph(
  slug: string,
  graph: Graph,
  signal?: AbortSignal,
): Promise<Graph> {
  return request<Graph>("PUT", `/api/projects/${encodeURIComponent(slug)}/graph`, {
    body: graph,
    signal,
  });
}

// ---- Build orchestration (Section 6) ---------------------------------------

export interface ParsedDiagnostic {
  file_path: string;
  line: number;
  column: number;
  severity: string; // "error" | "warning"
  message: string;
  code?: string;
  node_id?: string;
}

export interface BuildEvent {
  stream: "start" | "stdout" | "stderr" | "exit" | "diagnostic";
  line?: string;
  command?: string;
  code?: number;
  diagnostic?: ParsedDiagnostic;
}


/// Convert the HTTP API base to a WebSocket base.
function wsBase(): string {
  return API_BASE.replace(/^http/, "ws");
}

export function buildWebSocketUrl(slug: string): string {
  return `${wsBase()}/ws/build/${encodeURIComponent(slug)}`;
}

/// `POST /api/projects/:slug/build` — trigger a cargo check (or cargo build
/// --release when `release` is true). Returns 202; output streams over WS.
export async function triggerBuild(
  slug: string,
  release = false,
  signal?: AbortSignal,
): Promise<void> {
  const params = release ? "?release=true" : "";
  await request<void>("POST", `/api/projects/${encodeURIComponent(slug)}/build${params}`, {
    signal,
    expectEmptyBody: true,
  });
}

export interface PerformanceStats {
  throughput: number;
  avg_latency_us: number;
  p99_latency_us: number;
}

export interface RunEvent {
  stream: "start" | "stdout" | "stderr" | "exit" | "stop" | "metrics" | "debug_state";
  line?: string;
  command?: string;
  code?: number | null;
  reason?: string;
  node_id?: string;
  state?: string;
  value?: string;
  metrics?: Record<string, PerformanceStats>;
}

export interface RunStatus {
  running: boolean;
  slug: string;
}

export function runWebSocketUrl(slug: string): string {
  return `${wsBase()}/ws/run/${encodeURIComponent(slug)}`;
}

export async function triggerRun(slug: string, signal?: AbortSignal): Promise<void> {
  await request<void>("POST", `/api/projects/${encodeURIComponent(slug)}/run`, {
    signal,
    expectEmptyBody: true,
  });
}

export async function stopRun(slug: string, signal?: AbortSignal): Promise<void> {
  await request<void>("POST", `/api/projects/${encodeURIComponent(slug)}/stop`, {
    signal,
    expectEmptyBody: true,
  });
}

export async function triggerTest(slug: string, signal?: AbortSignal): Promise<void> {
  await request<void>("POST", `/api/projects/${encodeURIComponent(slug)}/test`, {
    signal,
    expectEmptyBody: true,
  });
}

export async function triggerDebug(
  slug: string,
  breakpoints?: string[],
  signal?: AbortSignal,
): Promise<void> {
  await request<void>("POST", `/api/projects/${encodeURIComponent(slug)}/debug`, {
    body: breakpoints ? { breakpoints } : undefined,
    signal,
    expectEmptyBody: true,
  });
}

export async function sendDebugAction(
  slug: string,
  action: "resume" | "step",
  signal?: AbortSignal,
): Promise<void> {
  await request<void>("POST", `/api/projects/${encodeURIComponent(slug)}/debug/action`, {
    body: { action },
    signal,
    expectEmptyBody: true,
  });
}


export async function fetchRunStatus(slug: string, signal?: AbortSignal): Promise<RunStatus> {
  return request<RunStatus>("GET", `/api/projects/${encodeURIComponent(slug)}/status`, {
    signal,
  });
}

export interface ChatMessage {
  role: "user" | "assistant";
  content: string;
}

/// `POST /api/projects/:slug/llm/generate-flow` — completely generate or heavily modify flow graph via LLM.
export async function generateFlow(
  slug: string,
  prompt: string,
  history?: ChatMessage[],
  signal?: AbortSignal,
): Promise<Graph> {
  return request<Graph>("POST", `/api/projects/${encodeURIComponent(slug)}/llm/generate-flow`, {
    body: { prompt, history },
    signal,
  });
}

/// `POST /api/projects/:slug/llm/refine-flow` — make minor refinements or tweaks to the visual graph via LLM.
export async function refineFlow(
  slug: string,
  prompt: string,
  history?: ChatMessage[],
  signal?: AbortSignal,
): Promise<Graph> {
  return request<Graph>("POST", `/api/projects/${encodeURIComponent(slug)}/llm/refine-flow`, {
    body: { prompt, history },
    signal,
  });
}

export interface VulnerabilityReport {
  crate_name: string;
  version: string;
  advisory_id: string;
  summary: string;
  severity: string;
  url: string;
}

export interface SecretLeak {
  node_id: string;
  field: string;
  secret_type: string;
  masked_value: string;
  message: string;
}

export interface SecureCodeViolation {
  node_id: string;
  violation_type: string;
  message: string;
  severity: string;
  advice: string;
}

export interface SecurityAuditReport {
  vulnerabilities: VulnerabilityReport[];
  leaked_secrets: SecretLeak[];
  secure_code_violations: SecureCodeViolation[];
  security_score: number;
}

/// `POST /api/projects/:slug/audit` — run security audit static analysis scans on the project
export async function runSecurityAudit(
  slug: string,
  signal?: AbortSignal,
): Promise<SecurityAuditReport> {
  return request<SecurityAuditReport>(
    "POST",
    `/api/projects/${encodeURIComponent(slug)}/audit`,
    { signal }
  );
}

export interface DbColumn {
  name: string;
  data_type: string;
  nullable: boolean;
  primary_key: boolean;
}

export interface DbRelation {
  column: string;
  referenced_table: string;
  referenced_column: string;
}

export interface DbTable {
  name: string;
  columns: DbColumn[];
  relations: DbRelation[];
}

export interface DbSchemaReport {
  tables: DbTable[];
}

/// `POST /api/projects/:slug/db/schema` — introspect a database schema from connection string.
export async function fetchDbSchema(
  slug: string,
  connectionString: string,
  signal?: AbortSignal,
): Promise<DbSchemaReport> {
  return request<DbSchemaReport>(
    "POST",
    `/api/projects/${encodeURIComponent(slug)}/db/schema`,
    {
      body: { connection_string: connectionString },
      signal,
    }
  );
}


