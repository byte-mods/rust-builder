import { useEffect, useState } from "react";
import { ApiError, fetchHealth, type HealthResponse } from "./api";
import ProjectList from "./views/ProjectList";
import ProjectCanvas from "./views/ProjectCanvas";

// View state for the top-level router. Kept as a discriminated union so
// TypeScript catches missing branches when new views land in later sections
// (chat panel S11, step debugger S13).
type View =
  | { kind: "list" }
  | { kind: "canvas"; slug: string };

// Backend connectivity state. Renders as a coloured badge in the header.
type BackendState =
  | { kind: "checking" }
  | { kind: "ok"; body: HealthResponse }
  | { kind: "unreachable"; error: string };

/// Studio root.
///
/// Owns:
/// - `view` — which top-level view is showing (`ProjectList` vs
///   `ProjectCanvas`). Replaced by a real router (React Router or wouter)
///   if URL-shareable routes become a requirement; v1 keeps it in-memory.
/// - `backendState` — the result of the `/health` probe, surfaced as a
///   permanent header badge so disconnects are immediately visible.
export default function App(): JSX.Element {
  const [view, setView] = useState<View>({ kind: "list" });
  const [backendState, setBackendState] = useState<BackendState>({ kind: "checking" });

  useEffect(() => {
    const controller = new AbortController();
    fetchHealth(controller.signal)
      .then((body) => setBackendState({ kind: "ok", body }))
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === "AbortError") return;
        const message =
          err instanceof ApiError ? err.message : err instanceof Error ? err.message : String(err);
        setBackendState({ kind: "unreachable", error: message });
      });
    return () => controller.abort();
  }, []);

  return (
    <div className="studio-root">
      <header className="studio-header">
        <h1>rust_no_code studio</h1>
        <BackendBadge state={backendState} />
      </header>
      {view.kind === "list" && (
        <ProjectList onOpen={(slug) => setView({ kind: "canvas", slug })} />
      )}
      {view.kind === "canvas" && (
        <ProjectCanvas
          slug={view.slug}
          onBack={() => setView({ kind: "list" })}
        />
      )}
    </div>
  );
}

function BackendBadge({ state }: { state: BackendState }): JSX.Element {
  switch (state.kind) {
    case "checking":
      return <span className="badge badge-checking">backend: checking…</span>;
    case "ok":
      return (
        <span className="badge badge-ok">
          backend: ok · v{state.body.version}
        </span>
      );
    case "unreachable":
      return (
        <span className="badge badge-down" title={state.error}>
          backend: unreachable
        </span>
      );
  }
}
