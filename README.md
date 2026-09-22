# DAGOS

A minimal DAG operating system for LLM coding workflows.

```text
user message → run → Jev classification → active context → versioned IR
  → inference provider → structured JSON response → validated DAG emissions → durable DAG → events
```

- **DAG** — durable project state (SQLite).
- **Jev** — a context classifier. It decides which DAG nodes are active; it never plans, routes, or acts.
- **Active context** — a temporary, per-run projection of the DAG. Removing context never deletes a node.
- **IR** — the versioned boundary between DAGOS and inference providers. Providers never see raw DAG records.
- **Provider** — an interchangeable inference endpoint.
- **Structured response** — validated JSON; presentation prose is kept apart from canonical emissions.
- **Events** — the ordered, inspectable history of every run.

The design lives in [`.specify/memory/constitution.md`](.specify/memory/constitution.md) and
[`specs/001-core-runtime`](specs/001-core-runtime); machine contracts are in
[`specs/001-core-runtime/contracts`](specs/001-core-runtime/contracts).

## Layout

| path                 | role                                                                  |
|----------------------|-----------------------------------------------------------------------|
| `crates/dagos-core`  | domain, contracts, store, context (Jev), IR, provider trait, runtime  |
| `crates/dagos`       | transport: the `dagos` command-line interface                         |

`dagos-core` never depends on HTTP stacks or provider SDKs; `crates/dagos-core/tests/boundaries.rs`
enforces the layer rules.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```
