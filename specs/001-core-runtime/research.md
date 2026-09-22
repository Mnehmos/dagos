# Research: DAGOS Core Runtime

## Decisions
Use Rust, SQLite, serde/serde_json, JSON Schema validation, and an async HTTP layer for providers.

The DAG is canonical durable state. Active context is a projection. A run reads active context, compiles IR, invokes a provider, validates the final response, and converts valid emissions back into DAG mutations.

Jev is a narrow classifier endpoint. Its contract contains node IDs and classifications only. It cannot return plans, provider choices, tool calls, or actions.

Provider SDK details remain inside provider adapters. The core sees only the provider interface and versioned IR.

Streaming deltas are persisted as ordered events. Partial prose is presentation state. Only a validated final response can create semantic emissions.

Failures produce explicit error events. State already committed before failure remains intact.

## Spec Kit
The current Spec Kit workflow is constitution, specify, clarify, plan, checklist, tasks, analyze, implement, and converge. The full quality-gated path is appropriate for DAGOS because the architecture has meaningful boundaries and invariants.