# ⚡ rust_no_code — Enterprise Visual IDE for Bare-Metal Async Rust Services

`rust_no_code` is a state-of-the-art, production-grade visual IDE that enables developers to architect, build, compile, and debug complex, high-performance, asynchronous Rust services in the browser. 

The studio translates visual graphs directly into **idiomatic, unwrap-free, async Rust source code** powered by the **Tokio asynchronous runtime** and the **sqlx/rdkafka** ecosystem. The output is a standard, fully structured Cargo workspace with **zero-overhead runtime abstractions**, performing at the exact same level as hand-written, bare-metal Rust code.

*Think TIBCO StreamBase meets AWS Lambda Designer meets Tokio concurrency — built for systems engineers who require absolute speed, memory safety, and visual clarity.*

---

## 🚀 Key Architectural Pillars & Functionalities

`rust_no_code` provides ten fully wired, enterprise-grade components designed to take Rust services from visual designs to highly optimized production runtimes:

### 1. Visual Rust Core Language & Control-Flow Engine
Draggable, neomorphic canvas nodes that represent fundamental Rust language constructs. They compile to clean, structured, standard Rust code.
*   **`language.struct` (Struct Builder):** Design complex schemas, field types, and access visibilities. Generates standard structs in `src/types/` that automatically derive necessary traits (`Debug`, `Clone`, `Serialize`, `Deserialize`, `Default`).
*   **`language.enum` (Enum Builder):** Define Rust enums supporting Unit, Tuple, and Struct variant payloads with full compiler safety and clean serialization.
*   **`language.fn` (Function Builder):** Visual constructor for generic, lifetime-bounded, `async`, and `unsafe` Rust functions mapped to `src/functions/`.
*   **Control Flow Nodes:** Drag-and-drop structural logic templates including `language.if_else`, `language.match`, `language.loop`, `language.propagate` (`?` operator), `language.await`, and `language.pointer` for clean AST code generation.

### 2. Tokio Asynchronous Runtime Primitives
Visual components mapped to Tokio's industry-standard async runtime, enabling the composition of highly concurrent, non-blocking architectures:
*   **Multi-Task Spawning (`tokio.spawn`, `tokio.spawn_blocking`):** Spawns active green threads (`tokio::spawn(async move { ... })`) or dedicated OS threads for CPU-bound sync work with automatic boundary closures.
*   **High-Speed Channels (`tokio.mpsc`, `tokio.broadcast`):** Generates byte-stable channel constructor helpers (`make_<name>()`) with static generic bounds and compile-time capacity checking.
*   **Locks & Coordination (`tokio.mutex`, `tokio.rwlock`, `tokio.semaphore`, `tokio.notify`):** Instantiates async-aware locks and coordinates access to shared state across multi-task boundaries.
*   **Clocks & Time-Selectors (`tokio.sleep`, `tokio.interval`, `tokio.select`, `tokio.join`):** Visually coordinates async delays, repeating tickers, and concurrent branch races.

### 3. Dataflow & Multi-Task Ownership Inference
A compiler-level optimization pass that manages memory safety visually without introducing garbage collection or unnecessary cloning overhead:
*   **Automatic Thread-Sharing Wrappers:** The `DataflowGraph::analyze` compiler pass automatically wraps shared variables crossing `tokio::spawn` task boundaries in thread-safe, reference-counted `Arc` pointers.
*   **Automatic Mutex/RwLock Locking:** Upgrades shared, cross-task variables to `Arc<tokio::sync::RwLock<T>>` if mutated, automatically injecting `.read().await` and `.write().await` locks.
*   **Zero-Clone Lifetime Optimization:** Statically analyzes single-use vs. multi-use edge lifetimes. Single-use variables are moved (`Move` binding), whereas multi-use variables are borrowed (`Borrow` binding) to achieve native speed.

### 4. Ingest & Egress Universal Connectors
Production-ready connectors that wire the visual graph directly to modern storage layers and streaming backbones:
*   **Kafka Stream Broker:** High-performance, multi-threaded `IntegrationKafkaConsumer` (polling loop with downstream dispatch) and `IntegrationKafkaProducer` built on `rdkafka`.
*   **PostgreSQL Connector:** Fully typed connection pools (`sqlx`) that bind parameters and map rows to struct definitions.
*   **Redis Cache Database:** Generates fully typed, highly efficient `GET` and `SET` cache operators.
*   **SQLite Database:** Emits local disk-persisted `rusqlite` / `tokio-rusqlite` transactional executors.
*   **Cron Scheduler & File Tail:** Integrates background timezone-aware cron execution and async line-by-line file tails.

### 5. Custom Node SDK (Monaco & static Syn parser)
Allows power developers to write custom Rust logic inside the IDE, merging custom blocks with the visual code generation flow:
*   **Interactive Monaco Code Editor:** Provides a premium syntax-highlighted code drawer equipped with full autocompletion and shortcuts.
*   **Static AST Introspection:** On save, the backend parses the Monaco block using the `syn` AST parser. It extracts function arguments and return types to dynamically generate the node's visual ports.
*   **Compilation Guards:** Automatically intercepts and blocks dangerous panic expressions (`.unwrap()`, `.expect()`, `panic!`), enforcing safe, unwrap-free error propagation.

### 6. Visual Step Debugger & Pause Controller
A first-class visual debugger that allows engineers to step through, suspend, and analyze graph execution:
*   **Synchronous Suspension Bridge:** AST instrumentation wraps nodes inside an immediately invoked closure equipped with `bridge_before` and `bridge_after` hooks. If a debug session is active, executing threads are suspended via stdin read loops.
*   **Floating Controls HUD:** Renders a gorgeous neomorphic debugger dashboard carrying *Resume (F8)*, *Step Over (F10)*, and *Stop (Esc)*.
*   **Canvas Overlays:** Paused nodes glow in a gorgeous neomorphic cyan aura, wires trace active paths with pulsing neon particles, and edge labels display neomorphic JSON value capsules carrying live variables.

### 7. Real-Time Diagnostics & Compiler Squiggles
Integrates compiler feedback directly onto the canvas, mapping compile errors precisely to visual nodes:
*   **Structured Cargo JSON Parsing:** Intercepts `cargo check` JSON streams, extracting exact file spans, error levels, and source targets.
*   **Diagnostics-to-Node Mapper:** Maps file locations (e.g. `src/functions/my_fn.rs`) back to their emitting visual node IDs and broadcasts them over WebSockets.
*   **Visual Warning/Error Indicators:** Badges nodes with warning (yellow) or error (red) tags. Projects red squiggled underlines directly under offending Monaco lines.

### 8. High-Performance Performance Profiler
Monitors throughput, execution latencies, and stream backpressures in real time:
*   **Monotonic Probe Clocks:** Samples `Instant::now()` at node entry/exit. Execution overhead is compiled out in production builds.
*   **Metrics Aggregation Daemon:** Aggregates profile streams to calculate rolling events/sec, average latencies, and P99 latency percentiles every 1000ms.
*   **Animated Flow Streams:** Wires glow in emerald green and animate with high-speed particles based on throughput. Nodes render floating pills showing current speeds (e.g., `⚡ 1,240/s · ⏱ 85µs`).

### 9. Security Audit & OSV Scanner
Visual security tool checking dependencies and code for vulnerabilities:
*   **OSV.dev API Vulnerability Scanner:** Parses the project's `Cargo.lock` and queries the OSV.dev batch API for known crate vulnerabilities.
*   **Secrets Leakage Detector:** Checks canvas configuration parameters for leaked AWS keys, private credentials, database passwords, and Slack webhooks.
*   **OWASP Secure Code Checker:** Analyzes Monaco blocks for OWASP violations, such as raw SQL string formatting (SQL Injection risks) and weak cryptographic hashes.
*   **Radial SVG Security Score HUD:** Displays a neomorphic radial score indicator (0–100) and focuses the canvas onto offending nodes on click.

### 10. Complex Event Processing (CEP) & Streaming Engine
A powerful, high-throughput streaming engine for real-time event analytics and windowing operations:
*   **Stream Operators:** Draggable nodes for `stream.filter` (dynamic predicates with parallel task execution), `stream.map` (event mapping), and `stream.select` (projection).
*   **Binary Stream Operators:** `stream.union` (merges multiple streams using `tokio::select!`) and `stream.join` (joins left/right events inside sliding time windows).
*   **Windowing Aggregator (`stream.window`):** Tumbling and sliding buffers based on Count or Time, calculating rolling aggregates (`COUNT`, `SUM`, `AVG`, `MIN`, `MAX`).
*   **CEP Pattern Automata (`stream.pattern`):** A Finite State Automaton sequence matcher for detecting complex temporal patterns (e.g., "A followed by B within X seconds").
*   **OnceLock Channel Preservation:** Employs static `OnceLock` cells containing `Arc<Mutex<Receiver<T>>>`. If a supervising async worker panics, it restarts, re-acquires the lock, and resumes consuming without dropping channel events.

---

## 📂 Repository Layout

```text
rust-builder/
├── backend/            # Studio Backend Server (Axum, Tokio, Code Generators, Build Orchestrator)
│   ├── src/
│   │   ├── build/      # Background Cargo build executor and error parsing
│   │   ├── codegen/    # AST code generation, dataflow analyser, cache managers
│   │   ├── projects/   # REST CRUD endpoints, project store, security scanner
│   │   ├── run/        # Execution runner, performance stats parser
│   │   └── templates/  # Codegen templates and builtin node definitions (connectors, stream, etc.)
│   └── tests/          # Intensive E2E compiler smoke and API integration tests
├── frontend/           # Studio Visual UI (Vite + React + TS + ReactFlow + Monaco)
│   ├── src/
│   │   ├── components/ # Common UI components (Monaco Editor, visual blocks)
│   │   ├── views/      # Page views (ProjectCanvas, ProjectList, SecurityDrawer, etc.)
│   │   └── App.css     # Neomorphic glowing dark-mode styling variables
├── projects/           # Stored user projects (Each folder is a standalone, valid Rust crate)
├── templates/          # Standard template assets
├── CLAUDE.md           # Quick commands ledger for developers
├── features.md         # Production readiness audit file
└── README.md           # This comprehensive guide
```

---

## 🛠️ Installation & Setup

Follow these steps to set up, build, and run the `rust_no_code` IDE locally.

### 1. Prerequisites
Ensure the following tools are installed on your development machine:
*   **Rust & Cargo:** (v1.75+ recommended for OnceLock stability)
    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```
*   **Node.js & npm:** (v18+ recommended)
    ```bash
    # Verify installations
    node --version
    npm --version
    ```
*   *(Optional)* **Docker / Podman:** For launching external streaming systems (Kafka, Redis, PostgreSQL) to test full pipelines.

---

### 2. Backend Server Installation
1.  Navigate into the `backend/` directory:
    ```bash
    cd backend
    ```
2.  Install all backend dependencies and compile the server:
    ```bash
    cargo build --release
    ```
3.  Launch the backend server:
    ```bash
    cargo run
    ```
    *The Axum server will start listening on `http://127.0.0.1:7878`.*

4.  Verify the backend health status:
    ```bash
    curl http://127.0.0.1:7878/health
    ```
    *Should return: `{"status":"ok","version":"..."}`.*

---

### 3. Frontend Visual UI Installation
1.  Navigate into the `frontend/` directory (in a second terminal):
    ```bash
    cd frontend
    ```
2.  Install React/TypeScript and Monaco dependencies:
    ```bash
    npm install
    ```
3.  Start the Vite neomorphic developmental server:
    ```bash
    npm run dev
    ```
    *The application will boot and be accessible at `http://127.0.0.1:5173`.*

4.  Open `http://127.0.0.1:5173` in any modern web browser to interact with the Studio dashboard.

---

### 4. Running the Comprehensive Test Suite
`rust_no_code` is backed by 287 automated unit, E2E integration, and compiler smoke tests checking everything from parsing to AST generation:

*   **Run all unit and project API tests:**
    ```bash
    cd backend
    cargo test
    ```
*   **Run the slower language compiler check smoke tests:**
    To run the comprehensive smoke tests which generate real user projects on disk and invoke `cargo check` directly to verify compiling output, run:
    ```bash
    cd backend
    LANG_SMOKE_CARGO_CHECK=1 cargo test --test language_smoke -- --include-ignored
    ```
    *This verifies that the visual code generator produces 100% compile-correct, safe Rust.*

---

## ⚙️ Environment Variables

The backend and frontend can be customized using environment variables:

| Variable | Description | Default |
| :--- | :--- | :--- |
| `RUST_NO_CODE_PROJECTS_ROOT` | Absolute path where generated user-projects will be saved on disk | `./projects` |
| `OSV_API_URL` | Endpoint of the Open Source Vulnerabilities database for CVE checking | `https://api.osv.dev/v1/querybatch` |
| `PORT` | Port the backend Axum server will bind to | `7878` |
| `RUST_LOG` | Rust log tracing level (`error`, `warn`, `info`, `debug`, `trace`) | `info` |

---

## 🧑‍💻 Developer E2E Workflow

Here is how to visually construct, compile, run, and profile a microservice:

1.  **Boot the Studio:** Startup both the backend (`cargo run`) and frontend (`npm run dev`) services. Open the UI.
2.  **Create a New Project:** Click "New Project" and specify a slug (e.g. `order-processor`). An empty workspace folder will be created at `projects/order-processor/`.
3.  **Construct the Pipeline:** 
    *   Drag a `language.struct` node to define an incoming event, e.g. `Order { id: u64, amount: f64 }`.
    *   Drag a `stream.filter` node to only allow orders with `amount > 100.0`.
    *   Drag a `tokio.mpsc` node to construct an event dispatch channel.
    *   Connect the output of the struct through the filter into the queue channel.
4.  **Save and Verify Codegen:** Click **Save**. The compiler statically analyzes the dataflow graph, infers task boundaries, automatically adds `Arc` and `RwLock` primitives, and generates pure Rust files.
5.  **Compile & Hot-Reload:** Click **Build**. Check the WebSocket console for real-time progress. Any compiler warnings or schema mismatches will project error squiggles under the offending nodes immediately.
6.  **Run with Profiling:** Click **Run**. The service boots in the background. The performance panel lights up, displaying metrics showing events/sec and latencies right on the nodes. Wires pulse colorfully based on load.
7.  **Run Security Audits:** Open the Security drawer. Check the OSV vulnerabilities database and OWASP reports. If any credentials are plain or any SQL query is formatted insecurely, the canvas zooms directly to the offending block for rectification.

---

## 🔒 Security & OWASP Standards
We take production stability and security extremely seriously. The generated code strictly enforces:
1.  **Zero Unwrapped Errors:** All code utilizes explicit error handling (`thiserror`) propagating failures with `?`. No runtime crashes or panic states.
2.  **No Code Injection:** Monaco blocks are scanned for manual database raw queries. All SQL interactions utilize parameterized binding via `sqlx` to prevent SQL Injection attacks.
3.  **Secrets Interception:** Real-time regex scan blocks saving any client parameters containing active API tokens, private keys, or passwords.

---

## 📜 License
Licensed under the Apache License, Version 2.0. See `LICENSE` for details.
