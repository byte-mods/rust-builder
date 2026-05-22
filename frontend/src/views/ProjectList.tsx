import { useEffect, useRef, useState } from "react";
import {
  ApiError,
  createProject,
  deleteProject,
  listProjects,
  type ProjectMeta,
} from "../api";

// Quick client-side slug check matching the backend rules. This is purely
// for fast UX feedback — the backend re-validates and is the source of
// truth, so this regex can be relaxed/refined without security implications.
const SLUG_RE = /^[a-z][a-z0-9-]{0,38}[a-z0-9]$/;

// Windows reserved device names. Rejected even on non-Windows hosts so a
// project folder copied to Windows (e.g. via git) does not fail to open.
const RESERVED_NAMES = new Set([
  "con", "prn", "aux", "nul",
  "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9",
  "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
]);

interface ProjectListProps {
  /** Asked to open a project's canvas. */
  onOpen: (slug: string) => void;
}

type FetchState =
  | { kind: "loading" }
  | { kind: "ready"; projects: ProjectMeta[] }
  | { kind: "error"; message: string };

/// Top-level studio view when no project is open. Lists every project in
/// the persistence layer, exposes a create form, and offers an "open" /
/// "delete" action per row.
export default function ProjectList({ onOpen }: ProjectListProps): JSX.Element {
  const [state, setState] = useState<FetchState>({ kind: "loading" });
  const [createSlug, setCreateSlug] = useState("");
  const [createName, setCreateName] = useState("");
  const [createBusy, setCreateBusy] = useState(false);
  const [createErr, setCreateErr] = useState<string | null>(null);
  const [refreshTick, setRefreshTick] = useState(0);
  const createAbortRef = useRef<AbortController | null>(null);

  // Load the list whenever the refresh tick advances (initial mount + every
  // mutation that should re-fetch). The abort guard is necessary in
  // StrictMode dev where effects run twice on mount.
  useEffect(() => {
    const controller = new AbortController();
    listProjects(controller.signal)
      .then((projects) => setState({ kind: "ready", projects }))
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === "AbortError") return;
        const message = err instanceof ApiError ? err.message : "unknown error";
        setState({ kind: "error", message });
      });
    return () => controller.abort();
  }, [refreshTick]);

  // Abort any in-flight create request when the component unmounts.
  useEffect(() => {
    return () => {
      createAbortRef.current?.abort();
    };
  }, []);

  function handleCreate(e: React.FormEvent<HTMLFormElement>): void {
    e.preventDefault();
    setCreateErr(null);
    if (!SLUG_RE.test(createSlug)) {
      setCreateErr(
        "slug must be 2-40 chars, lowercase, start with a letter, end with letter or digit, only [a-z0-9-]",
      );
      return;
    }
    if (RESERVED_NAMES.has(createSlug.toLowerCase())) {
      setCreateErr("slug is a reserved device name");
      return;
    }
    if (createName.trim().length === 0) {
      setCreateErr("name cannot be empty");
      return;
    }
    // Abort any previous in-flight create before starting a new one.
    createAbortRef.current?.abort();
    const controller = new AbortController();
    createAbortRef.current = controller;
    setCreateBusy(true);
    createProject(createSlug, createName.trim(), controller.signal)
      .then(() => {
        setCreateSlug("");
        setCreateName("");
        setRefreshTick((t) => t + 1);
      })
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === "AbortError") return;
        const message = err instanceof ApiError ? err.message : "create failed";
        setCreateErr(message);
      })
      .finally(() => {
        // Only clear busy state if this request is still the current one.
        // An aborted prior request must not clobber a newer in-flight create.
        if (createAbortRef.current === controller) {
          setCreateBusy(false);
          createAbortRef.current = null;
        }
      });
  }

  function handleDelete(slug: string): void {
    if (!window.confirm(`Delete project "${slug}"? This cannot be undone.`)) {
      return;
    }
    deleteProject(slug)
      .then(() => setRefreshTick((t) => t + 1))
      .catch((err: unknown) => {
        const message = err instanceof ApiError ? err.message : "delete failed";
        window.alert(`Delete failed: ${message}`);
      });
  }

  return (
    <div className="project-list">
      <section className="project-create">
        <h2>New project</h2>
        <form onSubmit={handleCreate}>
          <label>
            Slug
            <input
              type="text"
              value={createSlug}
              onChange={(e) => setCreateSlug(e.target.value)}
              placeholder="user-service"
              autoComplete="off"
              disabled={createBusy}
            />
          </label>
          <label>
            Name
            <input
              type="text"
              value={createName}
              onChange={(e) => setCreateName(e.target.value)}
              placeholder="User service"
              disabled={createBusy}
            />
          </label>
          <button type="submit" disabled={createBusy}>
            {createBusy ? "creating…" : "Create"}
          </button>
        </form>
        {createErr && <p className="form-error">{createErr}</p>}
      </section>

      <section className="project-table-wrap">
        <h2>Projects</h2>
        {state.kind === "loading" && <p className="muted">loading…</p>}
        {state.kind === "error" && (
          <p className="form-error">failed to load projects: {state.message}</p>
        )}
        {state.kind === "ready" && state.projects.length === 0 && (
          <p className="muted">no projects yet — create one above</p>
        )}
        {state.kind === "ready" && state.projects.length > 0 && (
          <table className="project-table">
            <thead>
              <tr>
                <th>Slug</th>
                <th>Name</th>
                <th>Updated</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {state.projects.map((p) => (
                <tr key={p.slug}>
                  <td><code>{p.slug}</code></td>
                  <td>{p.name}</td>
                  <td className="muted">{p.updated_at.slice(0, 19).replace("T", " ")}</td>
                  <td className="row-actions">
                    <button type="button" onClick={() => onOpen(p.slug)}>open</button>
                    <button
                      type="button"
                      className="danger"
                      onClick={() => handleDelete(p.slug)}
                    >
                      delete
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
    </div>
  );
}
