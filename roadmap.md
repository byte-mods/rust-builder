# Visual Backend Engineering Roadmap

Turning `rust_no_code` into a complete visual Rust IDE for backend engineering. The goal is **architecture-neutral primitives** — the studio gives the user every building block (packages, modules, functions, traits, impls, control flow, validation, DI, HTTP, consumers, schedulers, custom-code escape hatch) and the user composes them into whatever architecture suits the project. The studio does **not** enforce DDD, hexagonal, CQRS, MVC, or any other pattern. Users can build:

- Layered DDD (e.g. pounze-style: routes → services → traits → queries)
- Hexagonal / ports-and-adapters
- Flat microservice (one module, no layers)
- Event-sourced / CQRS
- Worker-only (no HTTP — just consumers, schedulers, batch jobs)
- Library crate (no `main.rs` at all)
- Multi-binary workspace
- Whatever else they invent

`~/projects/pounze_api` is used in this repo as **one worked example** to ensure the primitives are expressive enough to reproduce a real production backend — not as a target shape. Any roadmap item that smells pattern-prescriptive (e.g. "DDD layer") is a defect and should be reworded as a generic primitive.

Scope honesty: 10 sections, ~60 atomic tasks, multi-week. Foundation-first ordering — every later section attaches to the multi-package model from Section 1.

---

## Sections

| # | Section | Primitives delivered |
|---|---------|----------------------|
| 1 | **Multi-package project model** | A project is a tree of packages. Each package owns a graph fragment. Backend storage, CRUD, codegen emit nested modules. Frontend package-tree sidebar. **Architecture-neutral**: user decides the tree shape — flat, layered, workspace, library, whatever. |
| 2 | **Cross-package symbol import** | Every package exposes a public symbol surface (anything declared `pub`). Other packages reference symbols via import edges. Codegen emits `use crate::<path>::<sym>` automatically. Folds in deferred type-inference work. |
| 3 | **Module-item primitives** | `language.const`, `language.static`, `language.use`, `language.type_alias`, `language.mod` (in addition to existing struct/enum/fn). The full set of Rust top-level items as visual nodes. |
| 4 | **Control flow + data primitives** | Expand existing `language.if_else`/`match`/`loop` with `language.while`, `language.for`, `language.break`, `language.continue`, `language.return`, `language.let`, `language.assign`, `language.field_access`, `language.method_call`, `language.index`. Object parsing + validation utilities (`serde.parse`, `serde.serialize`, `validate.required`, `validate.regex`, `validate.range`) layered on top. |
| 5 | **DI / shared-state primitives** | `state.declare` (declares a shared-state container with arbitrary fields), `state.bind` (attaches a connector/value to a field), `state.extract` (handler-side pull). Framework-neutral: emits `web::Data<T>` for actix, `State<T>` for axum, `Arc<T>` for plain tokio. User chooses the umbrella shape. |
| 6 | **Trait + impl primitives** | `trait.def`, `trait.method`, `impl.block`, `impl.method`. Generic — user composes them into ports/adapters, repositories, strategy pattern, whatever. No "store" or "query" terminology baked in; those are *uses* of the primitives, not the primitives themselves. |
| 7 | **HTTP route + handler primitives** | `http.server` (chooses framework), `http.route` (path + method + handler), `http.handler` (body function), `http.middleware`. Works for actix and axum from the same graph. Service-layer composition is achieved by the user wiring handler nodes to function/method nodes — no enforced "service" concept. |
| 8 | **Framework targets** | Project setting picks the target framework (actix / axum / none-just-tokio / library-only). Same graph compiles to whichever target the user picks. |
| 9 | **Long-running task primitives** | `task.spawn` (any future), `task.supervisor` (restart on panic with backoff), `consumer.kafka`, `consumer.rabbitmq`, `scheduler.cron`, `scheduler.interval`. User composes into background workers, schedulers, daemons. |
| 10 | **End-to-end demos** | Build several reference projects from the UI with zero raw Rust, each in a different architectural style — e.g. (a) pounze-style layered actix-MongoDB API, (b) flat axum microservice, (c) worker-only Kafka consumer with Postgres sink, (d) library crate. Each demo validates the primitives compose into its style. |

**Note on framing:** sections 5–7 used to be titled "DI container", "Trait + Query nodes (DDD layer)", "Route + Service nodes". Those titles leaked the pounze architecture into the primitives. Renamed to emphasise that the primitives are architecture-neutral; the layered patterns are user choices, not studio defaults.

---

## Section 1 — Multi-package project model (in progress)

**Goal:** a project is no longer a flat single graph; it's a tree of packages, each with its own graph, each emitting its own Rust module. **The tree shape is fully user-controlled** — the studio neither prescribes nor presupposes any folder layout.

| T | Task | Files |
|---|------|-------|
| T1 | Backend data model: `Package { id, slug, parent_id, label }` + `Project.packages: Vec<Package>` with hand-rolled migration `Deserialize` (legacy single-graph projects → one root package `"main"`) | `backend/src/projects/types.rs` |
| T2 | Storage layout: `projects/<slug>/packages/<pkg-slug>/graph.json`. One-time on-disk migration: hoist existing `graph.json` → `packages/main/graph.json`. Atomic writes preserved | `backend/src/projects/store.rs` |
| T3 | CRUD HTTP endpoints: list/create/rename/delete package; per-package graph PUT/GET. Old single-graph endpoints stay as thin shims that target the root package for one release | `backend/src/projects/handlers.rs`, `backend/src/projects/mod.rs` |
| T4 | Codegen emits each package as a nested Rust module under `src/<path>/mod.rs`; root package becomes `lib.rs` (or `main.rs` for binary projects). No assumption about what *kind* of code each package contains | `backend/src/codegen/mod.rs`, `backend/src/codegen/bootstrap.rs` |
| T5 | Frontend: package-tree sidebar; click a package to load its graph; create/rename/delete UI. User freely names packages and nests them as they wish | `frontend/src/views/ProjectCanvas.tsx`, new `frontend/src/views/PackageTree.tsx`, `frontend/src/api.ts` |
| T6 | Integration tests: legacy single-graph project migrates cleanly; new multi-package project regens deterministically and `cargo check`s | `backend/tests/projects.rs`, `backend/tests/templates.rs` |

**Risks**
- Breaking existing projects on disk → T2 explicit one-time migration; T6 verifies a pre-migration project survives a load+save cycle.
- `store.rs` is 2200 LOC and central → additive changes only; existing single-graph helpers remain as the "root package" path until T4 ports them.
- Over-prescribing tree shape → the schema must allow any depth, any naming, any branching. Validate only that slugs are FS-safe and parent_id forms an acyclic tree.

**Deliverable**
- Backend supports an arbitrary user-defined package tree with full CRUD.
- Codegen produces a Rust source tree mirroring whatever the user built.
- Frontend shows the package tree; user can create/delete/rename/nest packages and switch between their graphs.
- Existing projects auto-migrate; all existing tests still pass.

---

## Architecture-neutrality test (gate for every section)

Before any section closes, the following thought experiment must hold:

> *Could a user use only the primitives shipped so far to build (a) a DDD-layered service, (b) a flat microservice, (c) a worker-only daemon, (d) a library crate — without the studio fighting them?*

If the answer is "no" for any of those four, the primitives are over-fitted to one pattern. Rework before closing.

---

## Deferred work (previously queued, folded into the new plan)

- *Type inference across edges* — original T1 already shipped (codegen::types). T2+ folds into Section 2 (cross-package symbol resolution needs the same TypeResolver foundation).
- *S19 Visual Test Runner* — defer; not blocking the visual-engineering roadmap.
- *S17 Framework Deep Customization drawer (middleware, CORS, rate-limit, hooks)* — partially absorbed by Section 7 + Section 8 (HTTP route node + framework target).
- *S20 Auto-fix suggestions* — defer.
- *Connector packs: messaging / databases / search & storage* — defer; Sections 5–6 introduce the generic DI/trait primitives, individual connectors land afterward at low cost.
- *Streaming gaps, Observability, Deployment, Type-system extras, Claude CLI per-project chat, S14 Final polish* — defer to post-Section-10.

---

## Reference example: pounze_api (one possible architecture, not the target)

Provided **only** as a worked example to validate the primitives are expressive enough for a real layered backend. Users are free to build something entirely different.

```
src/
├── main.rs                              tokio runtime + clap + tracing + actix bind
├── lib.rs
├── state.rs                             AppState { db_store: Arc<RwLock<Box<dyn CombinedDBStore>>> }
├── config/configuration.rs              etcd / file-based config loader
├── applications/
│   ├── connect_mongo.rs                 MongodbStore { client, app_name } + connect() + get_db()
│   ├── connect_kafka.rs
│   ├── connect_clickhouse.rs
│   └── application_store.rs             DataStoreSession { mongo_store } — the real CombinedDBStore impl
├── routes/
│   ├── create_connections.rs            HttpServer::new() — App builder with CORS, Compress, TracingLogger, .configure(...)
│   └── <domain>/<endpoint>.rs           thin actix handlers — pull store, call service, map result
├── services/<domain>/<svc>.rs           business logic; orchestrates trait methods
├── traits/
│   ├── super_traits/
│   │   ├── db_queries_combined.rs       CombinedDBStore umbrella + as_<domain>_store() accessors
│   │   └── mock_data_store.rs           DataMockStoreSession — test-time impl of every trait
│   └── <domain>_store/<trait>.rs        async_trait port (AuthStores, StoreStore, etc.)
├── queries/<domain>/*.rs                MongoDB adapters — impl of the traits
├── models/                              DTOs
├── validate_requests/                   request-validation helpers
└── utils/, templates/                   misc
```

Other valid shapes the studio must also support (without studio-side special cases):

```
src/lib.rs                               # library crate — no main.rs, no HTTP
src/foo.rs
src/bar.rs

src/main.rs                              # flat microservice — one file, no layers
src/handlers.rs

src/main.rs                              # worker-only — no HTTP, just consumers + schedulers
src/kafka_consumer.rs
src/cron_jobs.rs
src/db.rs

[workspace]                              # multi-binary workspace — N packages, each a crate
members = ["api", "worker", "shared"]
```

The primitives in Sections 1–9 must compose into all of the above.
