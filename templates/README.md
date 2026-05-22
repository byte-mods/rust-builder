# templates/

Codegen templates for node types. Populated from Section 3 (node template registry) onward.

Each node type lands here as a folder containing:

- `node.toml` — port spec, config schema, dependency hints.
- `*.tera` (or `*.hbs`, TBD in S3) — the source-text templates the codegen walks to produce Rust files in `projects/<slug>/src/`.

Section 1 reserves the directory; the schema and the first templates land in Section 3.
