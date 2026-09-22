# Research: DAGOS Core Runtime

## Decisions
Use Rust, SQLite, serde/serde_json, JSON Schema validation, and an async HTTP layer for providers.

The DAG is canonical durable state. Active context is a projection. A run reads active context, compiles IR, invokes a provider, validates the final response, and converts valid emissions back into DAG mutations.

Jev is a narrow classifier endpoint. Its contract contains node IDs and classifications only. It cannot return plans, provider choices, tool calls, or actions.

Provider SDK details remain inside provider adapters. The core sees only the provider interface and versioned IR.

Streaming deltas are persisted as ordered events. Partial prose is presentation state. Only a validated final response can create semantic emissions.

Assistant prose, partial or final, never becomes DAG state: it persists only in the run's event history. The run's user message is recorded as a `conversation` node because it is the run's request (and the IR task). Anything a model needs remembered it must emit as structured nodes and edges. Emission edges may only touch refs declared in the same response or nodes that were present in the IR; DAG rules that depend on live state (duplicate edges, cycles) are enforced when emissions are applied, atomically.

Failures produce explicit error events. State already committed before failure remains intact.

## Spec Kit
The current Spec Kit workflow is constitution, specify, clarify, plan, checklist, tasks, analyze, implement, and converge. The full quality-gated path is appropriate for DAGOS because the architecture has meaningful boundaries and invariants.
MCP is an optional capability boundary. A workspace may list stdio MCP servers; DAGOS asks each for its tools (`initialize`, `tools/list`) with a per-server time limit and compiles the results into IR `tools` as descriptive `<server>.<tool>` entries. Discovery failures are reported and skipped, so runs never depend on MCP. Requested `tool_calls` are recorded in the validated response and never executed in v0.1; no MCP transport or tool semantics enter DAG state or the core.
