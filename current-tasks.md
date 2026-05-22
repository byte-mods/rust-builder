# Current Tasks

_Single source of truth for in-flight work. Updated by the agent before starting and after completing every task. Read this first to see what is in progress, what is queued, and what is done._

## In progress

- [T1] Add `RunManager` module (`backend/src/run/mod.rs`) with start/stop/status + WebSocket log streaming — started 2026-05-22T00:00:31+05:30

## Queued

- [T2] Wire run endpoints into `projects_router` and update `AppState` with `run_manager`
- [T3] Add `RunError` → `ApiError` mapping and extend error surface
- [T4] Generate per-project `CLAUDE.md` during codegen (`backend/src/codegen/claude_md.rs`)
- [T5] Update frontend API client (`frontend/src/api.ts`) with run/stop/status + run WS URL
- [T6] Integration tests for run lifecycle (start, stop, status, WS stream, idempotency)
- [T7] Integration tests for `CLAUDE.md` generation (content accuracy, idempotency)

## Completed (this session)

_(none)_
