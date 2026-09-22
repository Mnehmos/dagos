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

## Quickstart (offline)

```bash
cargo run -p dagos -- init
cargo run -p dagos -- run "Where should durable state live?"
cargo run -p dagos -- run "Add restart tests"
```

The first run needs no network: it uses the deterministic fake Jev and the fake provider. Prose
streams to stdout; each run records the user message, the Jev classification, the compiled IR,
the provider output, and the validated emissions in `.dagos/dagos.sqlite3`.

The fake provider's model ID selects a behaviour, which makes every failure mode reproducible:
`fake-echo` (valid), `fake-malformed`, `fake-invalid-schema`, `fake-dangling-edge`, `fake-cycle`,
`fake-timeout`, `fake-error`. Failed runs stay inspectable and never mutate the DAG:

```bash
cargo run -p dagos -- run "Try this" --model fake-malformed
```

`dagos config` shows or changes the provider, model, and system prompt used by subsequent runs
(`--provider`, `--model`, `--system-prompt`, `--system-prompt-file`); `dagos run` accepts the same
flags as one-off overrides. Each run records the configuration it actually used.

### Real providers

Setting `OPENROUTER_API_KEY` or `OPENAI_API_KEY` enables the `openrouter` or `openai` provider.
Other OpenAI-compatible endpoints go in `.dagos/providers.json`:

```json
{"providers": [
  {"id": "ollama", "kind": "openai-compatible", "base_url": "http://localhost:11434/v1",
   "models": ["qwen2.5-coder:7b"]},
  {"id": "zai", "kind": "openai-compatible", "base_url": "<your Z.ai base URL>",
   "api_key_env": "ZAI_API_KEY", "models": ["<model id>"]}
]}
```

API keys are only read from the environment variable named by `api_key_env`; DAGOS never stores
them. Then, for example:

```bash
cargo run -p dagos -- run "Summarize the open tasks" --provider openrouter --model <model id>
```

## Layout

| path                  | role                                                                   |
|-----------------------|------------------------------------------------------------------------|
| `crates/dagos-core`   | domain, contracts, store, context (Jev), IR, provider trait, runtime   |
| `crates/dagos-openai` | inference adapter for OpenAI-compatible endpoints (OpenRouter, Ollama) |
| `crates/dagos`        | transport: the `dagos` command-line interface                          |

`dagos-core` never depends on HTTP stacks or provider SDKs; `crates/dagos-core/tests/boundaries.rs`
enforces the layer rules. Real providers live in their own crates and implement the core's
`InferenceProvider` trait.

To check a real endpoint (opt-in, needs network and usually a key):

```bash
DAGOS_LIVE_BASE_URL=https://openrouter.ai/api/v1 DAGOS_LIVE_MODEL=<model> DAGOS_LIVE_API_KEY=<key> cargo test -p dagos-openai -- --ignored
```

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```
