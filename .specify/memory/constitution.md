# DAGOS Constitution

## Project
DAGOS is a minimal operating system for LLM coding workflows. Durable project state is a DAG, active context is a temporary projection, Jev classifies context membership, and inference providers execute against a versioned intermediate representation.

## Principles
1. DAGOS is the runtime substrate, not an agent framework.
2. Codex, Claude Code, Z.ai, OpenRouter, and future providers are interchangeable inference endpoints.
3. Jev is only a classifier. It manages context membership and does not plan, route, act, recover, escalate, reason, or execute tools.
4. Durable DAG state is separate from active context. Removing context never deletes durable state.
5. Providers receive versioned IR, never raw DAG records.
6. All model input and output is machine-readable JSON. User prose is an explicit presentation field, not canonical state.
7. MCP is optional external capability infrastructure. DAGOS may execute MCP tool calls only as a bounded, person-controlled step of a run: a validated response requests the call, the tool's policy (off, ask, allow) or a person's explicit approval permits it, a fixed step limit bounds each run, every request, decision, and result is an event, and results reach the model only through the next IR. Tool output never becomes DAG state except through validated emissions.
8. KISS: SQLite, explicit interfaces, deterministic state transitions, no vectors, embeddings, autonomous orchestration, routing, or elaborate memory in v0.1.
9. Runs, classifications, context changes, inference events, and emissions must be inspectable.
10. Core contracts and state transitions require automated tests.

## Non-goals
Autonomous multi-agent orchestration, provider routing, embeddings, vector databases, provider-specific agent frameworks, automatic prompt optimization, hidden mutable memory, and GUI automation as core capability.

## Amendments
- 2026-09-23: Principle 7 allows person-controlled tool execution through MCP (requested by the project owner). DAGOS still does not plan, pursue goals, retry, or act on its own: every run starts from a person's message, and every tool call is either pre-permitted by that person's policy or approved by them.

- 2026-09-23: Jev also classifies which offered tools the model sees in a run (requested by the project owner): the same `active` / `inactive` membership labels, applied to tool candidates instead of nodes. It is still classification only: Jev never calls, permits, or orders tools; each tool's policy and the person still decide whether a call runs, and without a model Jev every offered tool stays exposed.
- 2026-09-23: Jev also judges relevance for history (requested by the project owner): one yes/no classification per earlier turn of the project's chats (relevant turns are recalled into the IR verbatim) and per large earlier tool result of a run (irrelevant ones are left out of the next IR). This replaces summarizing compaction. Jev still only classifies: DAGOS reads the history, decides nothing else on Jev's behalf, and never asks the model to recall.
- 2026-09-23: A bounded review loop (requested by the project owner). Jev judges the functions a run's tool calls changed against the project's plain-English lint rules (one yes/no question per rule and function). While a judgment applies, DAGOS hands the findings back to the model in the IR instead of completing the run, up to a configured number of reviews, and stops as soon as the code is clean or stops changing. Jev still only classifies: it never edits, plans, or decides to continue; the runtime's fixed rule does, and every tool call in the loop still passes the person's tool policy.
- 2026-09-23: Jev judges tool calls' risks (requested by the project owner): one yes/no question per risk for each call about to run. A likely risk can only escalate a call to the person; Jev never allows, denies, or runs anything, and a failed check escalates too.
- 2026-09-24: No count caps on context (requested by the project owner: "management, not restraint"). Everything Jev judges relevant goes in; the model's window is the only limit, handled by leaving out the least relevant first. Jev also classifies whether the person's own message asks for all context; the runtime then loads everything. Jev still only classifies.

## Change Rule
Expanding Jev beyond classification or turning DAGOS into an autonomous agent framework requires an explicit constitution change.