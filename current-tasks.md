# Current Tasks

_Single source of truth for in-flight work. Updated by the agent before starting and after completing every task. Read this first to see what is in progress, what is queued, and what is done._

## In progress

- [Section 1 — Type inference across edges] — started 2026-05-24T00:50:00+05:30
  - [T1] codegen::types module + TypeResolver + ResolvedType + PortSide — completed 2026-05-24T01:05:00+05:30 — super-qa PASS, 7 new tests, full lib suite green (255 passed), 2 non-blocking MINORs noted.

## Queued

- [Section 2 — S19 Visual Test Runner]
- [Section 3 — S17 Framework Deep Customization drawer (middleware, CORS, rate-limit, hooks)]
- [Section 4 — S20 Auto-fix suggestions]
- [Section 5 — Connector pack: messaging family (Redis Pub/Sub, NATS, RabbitMQ, Pulsar)]
- [Section 6 — Connector pack: databases family (MongoDB, Scylla/Cassandra, ClickHouse, DynamoDB, MySQL writer)]
- [Section 7 — Connector pack: search & storage (Elasticsearch, Meilisearch, S3, Memcached, File/CSV writer, WebSocket server, UDP/TCP, CDC)]
- [Section 8 — Streaming gaps (Session/Delta/Punctuation windows, Absence + Aggregation-over-pattern + Sliding-pattern, watermarks/event-time)]
- [Section 9 — Observability (Prometheus metrics, /health probes, OpenTelemetry, alerting hooks)]
- [Section 10 — Deployment (Dockerfile gen, K8s manifest gen, env/secret management)]
- [Section 11 — Type-system extras (XML schema, FlatBuffers/Cap'n Proto, schema registry)]
- [Section 12 — S11 Claude CLI per-project chat]
- [Section 13 — S14 Final polish + demo flow]

## Completed (this session)

- [S23–S27 — Core Stream Processing (CEP Engine)] — completed 2026-05-24T00:30:00+05:30 — Designed and implemented all 7 stream-processing operators (`stream.filter`, `stream.map`, `stream.select`, `stream.union`, `stream.join`, `stream.window`, and `stream.pattern`) in `builtins/stream.rs`. Integrated standard high-performance `tokio::sync::mpsc` channels and supervisors with an Arc-Mutex static receiver preservation layer that recovers panicking tasks without losing events or senders. Wired optional dynamic thread-task spawning ("Gradon Parallelism") per event for infinite scaling. Registered all new templates in the builtins inventory and template summaries, wrote an E2E multi-operator compile test `test_cep_operators_pipeline_smoke` in `language_smoke.rs`, and verified all 280+ tests run 100% green.
- [S22 — Security Audit] — completed 2026-05-23T23:50:00+05:30 — Designed and implemented automated dependency CVE scanning using OSV.dev batch queries with offline resilience, regex-based API key/credentials/tokens and database connection leaks detection, static OWASP compliance secure coding checks (SQL Injection, cryptographic weak hashing A02, SSRF), Axum routing middlewares, a structured `/projects/:slug/audit` REST API, premium glassmorphic `SecurityDrawer.tsx` dashboard UI with glowing radial progress ring, letter grade shield, locate canvas node panning animations, and complete automated E2E tests.

- [S21 — Performance Profiler] — completed 2026-05-23T23:35:00+05:30 — Designed and implemented the High-Performance Canvas Profiler. Includes low-overhead monotonic clock probes inside user projects to measure execution latency, a backend sliding-window metrics aggregator that drains reports and computes averages and P99 percentiles every second to throttle updates to exactly 1 message/sec, glowing neon emerald green edge wire visual highlights carrying actual throughput counts, and dynamic glassmorphic performance badges overlaying events/sec and 99th percentile execution speeds directly on canvas nodes.
- [S13 — Visual Step Debugger] — completed 2026-05-23T23:10:00+05:30 — Built a fully synchronous, stdin-based process suspension bridge with dynamic AST instrumentation that wraps generated dynamic nodes in immediately invoked closures to prevent early-return hook bypasses. Intercepts structured logs, enables REST/WebSocket controls for Resume, Step Over, and Stop actions, maps animated glowing cyan edge tracer pulses and real-time hover value capsules, toggles crimson header breakpoints, and features a premium bottom-center glassmorphic control bar with global keyboard shortcuts (F8, F10, Escape).
- [S20 — Diagnostics Panel & Visual Cargo Compiler Errors] — completed 2026-05-23T21:40:00+05:30 — Wired unstructured rustc JSON compiler output parsing to dynamically map errors to source nodes, broadcasted diagnostic updates over WebSockets, rendered gorgeous floating warning and error tooltips directly on visual nodes, and projected instant red/yellow squiggly error highlights directly inside Monaco code editors.
- [S16/S17 — Custom Node SDK / Universal Connectors] — completed 2026-05-23T22:50:00+05:30 — Created a `custom.block` visual node template that allows developers to write raw Rust code directly inside Monaco on the canvas, statically parses function signature using `syn` to update dynamic canvas ports on graph-save, hides internal array configurations, and provides production-grade enterprise connectors for Kafka Consumer, Kafka Producer, Redis Cache, and PostgreSQL/SQL templates.
- [S10 — Input / Output Adapters (Ingest & Egress)] — completed 2026-05-23T22:30:00+05:30 — Built operational Cron Scheduler, File Tail Reader, HTTP Webhook Client, and SQLite Database Writer adapters with dynamic dataflow type bindings, full supervised Tokio spawn integrations, E2E Sub-Cargo compile check tests, and 100% green unit tests.
- [S12 — Run Lifecycle & CLAUDE.md] — completed 2026-05-23T22:45:00+05:30 — Completed visual run/stop execution handlers, WebSocket stdout/stderr event log streaming, and automatic per-project ASCII `CLAUDE.md` Visual Project Contract generation during code generation.
- [S15 — Visual Rust Core Language & Monaco Editor] — completed 2026-05-23T19:00:00+05:30 — Designed and implemented all 6 control-flow / pointer templates (`language.if_else`, `language.match`, `language.loop`, `language.propagate`, `language.await`, `language.pointer`), extended structs/enums/functions with lifetimes and trait bounds, implemented dynamic sorting of lifetimes, created the premium zero-dependency AMD CDN-loaded `MonacoEditor` component with system theme sync and markers, and wired Monaco Editor to replace string textareas in `SchemaForm.tsx`.
- [S19 T1 to S19 T5] LLM flow generation (Anthropic API via studio backend) — completed 2026-05-23T18:45:00+05:30 — Designed and implemented a project-wide context assembler capping at ~120,000 characters, a forced-tool-call Anthropic Messages API client targeting `update_graph` schema, backend POST handlers for `generate-flow` and `refine-flow` with TemplateRegistry post-validation, and a highly responsive React canvas review UI displaying color-coded visual diffs with strict read-only lock protection.
- [S18 T1 to S18 T6] Tokio runtime nodes — completed 2026-05-23T17:45:00+05:30 — Created and registered 10 new templates: `tokio.mutex`, `tokio.rwlock`, `tokio.spawn`, `tokio.sleep`, `tokio.interval`, `tokio.select`, `tokio.join`, `tokio.spawn_blocking`, `tokio.semaphore`, and `tokio.notify`. Optimized comment-based clone placeholder preservation using raw string formatting to prevent Rust compiler macro comment-stripping. All 247+ tests (unit & integration) pass 100% successfully.
- [S17 T1 to S17 T6] Dataflow + ownership/borrow inference — completed 2026-05-23T17:30:00+05:30 — Built whole-graph dataflow analyzer, cross-task boundary detection, and syntax placeholder replacement pass to automatically wrap cross-task values in `Arc` and substitute `/*[clone:x]*/x` comments.
- [T1] Add `language.struct` template — completed 2026-05-23T13:25:00+05:30 — super-qa PASS round 2; emits `[vis] struct Name {...}` to `src/types/<snake>.rs` with derive auto-serde-dep, byte-stable, validates against `syn::Ident` to prevent `format_ident!` panic on reserved keywords.
- [T2] Add `language.enum` template — completed 2026-05-23T14:00:00+05:30 — super-qa PASS round 1; emits Unit/Tuple/Struct-payload variants, same syn::Ident guard pattern, empty tuple/struct payloads flatten to Unit, empty variants list emits `enum X {}`.
- [T3] Add `language.fn` template — completed 2026-05-23T14:40:00+05:30 — super-qa PASS round 1; emits `[vis] [async] [unsafe] fn name<G: B>(p: T) -> R { body }` to `src/functions/<snake>.rs`, body parsed as `syn::Block`.
- [T4, T5] Folded into T1–T3 — registration and per-template tests landed inline.
- [T6] End-to-end smoke test — completed 2026-05-23T15:30:00+05:30 — super-qa PASS round 1. Two tests in `backend/tests/language_smoke.rs` confirming graph→Rust→compiling end-to-end.
- [S15b T1] Add `tokio.mpsc` template + new `tokio.rs` module — completed 2026-05-23T16:32:00+05:30 — super-qa PASS round 1. Emits `pub fn make_<name>() -> (mpsc::Sender<T>, mpsc::Receiver<T>) { mpsc::channel(N) }`.
- [S15b T2] Add `tokio.broadcast` template — completed 2026-05-23T16:48:00+05:30 — super-qa PASS round 1. Emits `pub fn make_<name>() -> (broadcast::Sender<T>, broadcast::Receiver<T>) { broadcast::channel(N) }`.
- [S16 T1] Graph-hash cache + `regen_if_changed` helper — completed 2026-05-23T17:48:00+05:30 — super-qa PASS round 1. New `backend/src/codegen/cache.rs` (~230 LOC + tests), `CodegenCache` on `AppState`, `delete_project` calls `forget`.
- [S16 T2] BuildManager verb extension — completed 2026-05-23T18:03:00+05:30 — super-qa PASS round 1. New `BuildVerb { Check, BuildRelease, Test }` enum with `argv` + `command_str`; `start_build` takes verb.
- [S16 T3] Wire endpoints (regen-first on /build /run; add /test /debug) — completed 2026-05-23T17:15:00+05:30 — super-qa PASS round 1.
- [S16 T4] Frontend: add `triggerRun` / `triggerTest` / `triggerDebug` + Run / Test / Debug buttons next to Check / Build Release. — completed 2026-05-23T17:20:00+05:30 — super-qa PASS.
