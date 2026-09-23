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

### Real providers and API keys

The easiest way is the app: run `dagos serve`, press **⚙** (or `s`), pick **OpenRouter**, **OpenAI**,
or **Z.ai**, paste a key, and press **Save & test**. DAGOS checks the key, lists the provider's
models, and lets you pick one for new runs. **+ Add endpoint** adds any other OpenAI-compatible
server (Ollama, LM Studio, vLLM, a gateway), with or without a key. Changes apply immediately.

Keys saved in the app live in one per-user file outside every project (`%APPDATA%\dagos\keys.json`
on Windows, `~/.config/dagos/keys.json` elsewhere, owner-only; `DAGOS_CONFIG_DIR` moves it). They
never enter the project, the database, the IR, or any API response: the app only ever shows a hint
such as `sk-o…7890`. From a terminal, `dagos keys set openrouter` reads a key from standard input,
and `dagos keys` / `dagos keys remove <provider>` list and remove them.

Environment variables still work and always win over a saved key: `OPENROUTER_API_KEY`,
`OPENAI_API_KEY`, `ZAI_API_KEY`. Custom endpoints are kept in `.dagos/providers.json`:

```json
{"providers": [
  {"id": "ollama", "kind": "openai-compatible", "base_url": "http://localhost:11434/v1",
   "models": ["qwen2.5-coder:7b"]},
  {"id": "together", "kind": "openai-compatible", "base_url": "https://api.together.xyz/v1",
   "api_key_env": "TOGETHER_API_KEY", "models": ["<model id>"]}
]}
```

Then, for example:

```bash
cargo run -p dagos -- run "Summarize the open tasks" --provider openrouter --model <model id>
```

### Jev: offline by default, better with a model

Jev decides each run's active context: which durable nodes the model sees. The offline policy
classifier always works. To let a model classify instead, choose **Jev → Model** in the app, or set
`"jev": {"provider": "openrouter", "model": "<model id>"}` in `.dagos/providers.json`, or
`DAGOS_JEV_PROVIDER` and `DAGOS_JEV_MODEL` (the environment wins). On OpenRouter the recommended
Jev is TypeSafe's `~typesafe/jev-latest`, a decisions model: DAGOS asks it one calibrated yes/no
question per candidate node through OpenRouter's Decisions API (`/api/alpha/decisions`) and turns
the answers into a `kiss.jev-context.v1` classification. Any other model classifies through Chat
Completions; a small, fast one is plenty.

Runs never depend on the model Jev. If it fails, times out, has no key, or answers with anything but
a valid `kiss.jev-context.v1` classification (plans, prose, unknown nodes, extra fields), the run
records why (`jev.rejected`, `jev.fallback`) and the offline policy classifies the same request.
The model's classification is applied as the context; it can never write to the DAG, answer, or
choose providers. Each run's `jev.requested` event records which classifier was asked, e.g.
`openrouter-jev:<model>`, and the inspector's Jev tab shows who classified and why.

### MCP (optional)

DAGOS never needs MCP. To describe MCP tools to models, list stdio servers in `.dagos/mcp.json`:

```json
{"servers": [{"id": "files", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "."]}]}
```

(On Windows use `npx.cmd`.) `dagos run` and `dagos serve` ask each server for its tools and compile
them into the IR's `tools` as `<id>.<tool>` descriptions. They are descriptive only: DAGOS v0.1
records requested `tool_calls` but never executes them. A server that is missing, crashes, hangs, or
misbehaves is reported as a warning and skipped, and the run proceeds without its tools.

## Layout

| path                  | role                                                                   |
|-----------------------|------------------------------------------------------------------------|
| `crates/dagos-core`   | domain, contracts, store, context (Jev), IR, provider trait, runtime   |
| `crates/dagos-openai` | inference adapter for OpenAI-compatible endpoints (OpenRouter, Ollama) |
| `crates/dagos-mcp`    | optional MCP tool discovery, compiled into IR capability descriptions  |
| `crates/dagos`        | transport: CLI, workspace setup, inspection views, local HTTP API      |

`dagos-core` never depends on HTTP stacks or provider SDKs; `crates/dagos-core/tests/boundaries.rs`
enforces the layer rules. Real providers live in their own crates and implement the core's
`InferenceProvider` trait.

To check a real endpoint (opt-in, needs network and usually a key):

```bash
DAGOS_LIVE_BASE_URL=https://openrouter.ai/api/v1 DAGOS_LIVE_MODEL=<model> DAGOS_LIVE_API_KEY=<key> cargo test -p dagos-openai -- --ignored
```

Add `DAGOS_LIVE_JEV_MODEL=<model>` to classify context with a live model too.

## Development

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```
