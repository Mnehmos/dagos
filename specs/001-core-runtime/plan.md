# Implementation Plan: DAGOS Core Runtime

Feature: 001-core-runtime

## Summary
Implement a local-first DAG runtime with SQLite persistence, Jev classifier adapter, versioned inference IR, provider adapter interface, structured response validation, event log, and a minimal runtime boundary.

Start with fake Jev and fake provider so the architecture is testable without external services.

Converged scope (0→1): on top of the core, projects hold conversations (chats); MCP tools run under person-controlled policies with a risk guard; a model Jev (TypeSafe's, through OpenRouter's Decisions API) also chooses each run's tools, recalls relevant earlier turns, leaves out stale tool results, reviews changed code against plain-English lint rules, and judges tool-call risks. Every one of those is a yes/no classification the runtime acts on by a fixed rule; without a model Jev each falls back to the plain behaviour.

## Technical Context
Language: Rust stable (edition 2024); UI in plain ES modules served by the binary
Storage: SQLite (append-only migrations 0001-0003)
Testing: Rust unit and integration tests with temporary databases and fake adapters; `node --test` for UI modules
Targets: Windows and Linux local development
Project type: local runtime/library with a CLI and a local web app
Constraints: local-first, provider-neutral, deterministic state transitions, no raw DAG crossing the provider boundary
v0.1 scale: one local workspace, many projects, one running run per project

## Architecture
```text
User Message -> Run -> Jev Classification (context, tools) -> Recall (earlier turns) -> IR Compiler
  -> Provider Adapter -> Structured Response -> DAG Emissions -> Event Log
       ^                      |
       |   tool round (<= 8): gate (policy, named tools, guard) -> MCP call -> tool_results
       |   review round (<= 3): lint changed functions -> review
       +----------------------+
```

## Crates and layers
- `dagos-core` (no HTTP, no provider SDKs; layer rules enforced by the `boundaries` test):
  domain, contracts, store, context (Jev adapter, projection, recall), ir, provider (trait and fake),
  response, tools (gate, executor, and reviewer interfaces), runtime.
- `dagos-openai`: OpenAI-compatible Chat Completions provider; model Jev over Chat Completions or the
  Decisions API.
- `dagos-mcp`: stdio MCP client, server pool, tool policies.
- `dagos-lint`: function extraction, lint rules, judging, and the review loop's reviewer.
- `dagos`: CLI, local HTTP API, app UI, workspace wiring, keys, approvals, and the tool guard.

## Phases
Phase 0: Rust project, SQLite bootstrap, JSON contracts, IDs, timestamps, CI.
Phase 1: DAG node/edge persistence, runs/events, active context, transaction tests.
Phase 2: Jev request/response, adapter, fake classifier, classification application, strict boundary tests.
Phase 3: IR v1, deterministic compiler, snapshots, proof that providers receive IR only.
Phase 4: Provider trait, fake streaming provider, delta events, final response validation, emissions, failure tests.
Phase 5: One real provider behind the common interface.
Phase 6: Read-only inspector for DAG, context, run, events, provider/model, IR, response.
Phase 7: restart tests, MCP-optional verification, Spec Kit analyze, implement, converge, v0.1 tag.
Phase 8 (converged scope): app UI, saved keys and providers, optional model Jev with fallback, conversations and projects, MCP tool execution with policies and approvals, Jev tool exposure, automatic recall and compaction, the lint review loop, and the tool guard.

## Constitution Gate
Jev remains classification-only: node labels, tool labels, and yes/no probabilities, never plans, text, or actions. Providers remain interchangeable. Raw DAG never crosses the provider interface. Context removal preserves durable state. Machine interfaces remain versioned JSON. Loops are bounded and amended into the constitution: tool rounds (person-controlled tools) and review rounds; the runtime decides by fixed rules, never Jev or the model.
