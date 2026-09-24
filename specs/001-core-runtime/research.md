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
MCP is an optional capability boundary. A workspace may list stdio MCP servers in `mcp.json`; DAGOS asks each for its tools with a per-server time limit and offers them as `<server>.<tool>` IR tools. Discovery failures are reported and skipped, so runs never depend on MCP. Since the constitution amendment for person-controlled tools, requested `tool_calls` run through a gate (policy `off`/`ask`/`allow`, approval in the app, a 10-minute timeout) and an executor, up to 8 rounds per run; tool output returns through the next IR and never becomes DAG state by itself.

## Decisions since the core
- **Jev as a bank of yes/no judgments.** TypeSafe's Jev is a decisions model: OpenRouter's Decisions API answers typed questions with calibrated probabilities, many in parallel, and generates no text. DAGOS uses it for everything beyond context membership, and the runtime acts on the answers by fixed rules: tool exposure (one question per tool), recall (per earlier turn), compaction (per large tool result), lint review (per rule and function), and the tool guard (per risk). Each falls back to the plain behaviour when the Jev cannot answer.
- **Management, not restraint.** Coding often needs a lot of context, so nothing is capped by a count: every node Jev classifies active and every earlier turn it judges relevant goes in, whole. Jev's own reading limit (a trimmed copy of a long turn) never limits what the model receives. The one real limit is the model's context window (as the endpoint's model list reports it): when everything relevant does not fit, each step's IR is fitted to it, leaving out the least relevant recalled turns first, then earlier rounds' tool outputs (oldest first), then the oldest chat turns; nothing is left out while everything fits.
- **Recall instead of summaries.** Summaries lose what they leave out. Every turn stays in the event history; Jev decides per turn whether it is relevant, and relevant turns reach the IR verbatim. Recall is automatic, not a tool the model must think to call, and spans every chat of the project: chats organize work for people, the project is what agents remember.
- **The tool-call boundary is the hook.** A model reaches files and the system only through tool calls, and DAGOS sees each call before it runs. The review loop remembers the files a call names there; the guard judges the call there. No git state or repository snapshots are needed.
- **Checks only tighten.** Meta-tools inherit the strictest policy of the tools they name; the guard can only turn `allow` into a question. Deletion, secrets, and delegation to tools chosen at run time always ask, even when requested.
- **A small extractor over a parser.** Function extraction masks strings and comments, finds headers with regular expressions, and matches braces or indentation (Rust, JavaScript, TypeScript, Python). It avoids native parser dependencies at the cost of edge cases a full parser would handle.
- **Native tool calling behind the same contract.** The OpenAI-compatible adapter offers the IR's tools through the endpoint's `tools` field and turns native tool calls back into the response document's `tool_calls`; the runtime, gate, and guard cannot tell the difference, and providers stay interchangeable.
- **Tolerating chat-model quirks without trusting them.** A whole-output code fence, or a short plain-text preamble before a response document that starts on its own line, is removed before validation; anything else still fails closed.

## Unresolved tradeoffs
- The guard sees only what a call's arguments show: a shell command's own side effects are judged from its text. Files a call changes without naming them reach the review loop only through their modification time, judged whole (without an earlier copy to compare), and a file someone else edits during the run is included too.
- Jev's probabilities are calibrated but not infallible: the guard produced a harmless false positive (`outside-project` on a path inside the project written with backslashes), and lint findings can be wrong. Findings are handed back for the model to fix or dispute, never applied automatically.
- The review cache is saved in `.dagos/lint-cache.json`; the reviewer's per-run file memory is in-process (a restarted run is failed as interrupted anyway).
- Tool calls go through providers' native tool calling where the model takes it (falling back to DAGOS's JSON protocol for a model that refuses, remembered per model). Final replies still use the JSON document, since emissions need it, so a model that adds text around its final document is still handled by the preamble rule.
- Findings a run ends with become `observation` nodes (`kind: "lint_finding"`) that DAGOS records
  itself, the one place durable state is written without a model emission. They are not removed
  when the code is later fixed; Jev's classification keeps stale ones out of context.
