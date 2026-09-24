# Traceability: requirements → implementation → tests

Produced by the analyze/converge pass (#21). Every functional requirement has an implementation path
and automated acceptance coverage; `cargo test --workspace` and `npm test` (in `crates/dagos/ui`) run
all of it.

| FR | Implementation | Tests |
|---|---|---|
| 001 DAG persistence | `dagos-core/src/store/dag.rs` | `store_dag.rs`, `store/schema_tests.rs` |
| 002 node types | `domain/dag.rs` (closed enum, SQL CHECK) | `store_dag.rs`, `contracts.rs` |
| 003 edge types | `domain/dag.rs` | `store_dag.rs`, `contracts.rs` |
| 004 active context separate | `store/context.rs`, `context/projection.rs` | `context_projection.rs` |
| 005 removal keeps nodes | `context/projection.rs` | `context_projection.rs`, `jev_fake.rs` |
| 006 Jev request | `domain/jev.rs`, `contracts/jev-request.schema.json` | `jev_contract.rs` |
| 007 Jev classification only | `context/projection.rs` (`validate_classification`) | `jev_contract.rs`, `jev_fake.rs` |
| 008 versioned IR | `ir.rs`, `contracts/inference-ir.schema.json` | `ir_compile.rs`, `ir_contract.rs` |
| 009 providers receive IR only | `provider.rs` (trait takes `InferenceIr`) | `provider_boundary.rs`, `boundaries.rs` |
| 010 JSON provider input | `dagos-openai/src/protocol.rs` | `mock_endpoint.rs` |
| 011 JSON output: prose + emissions | `response.rs`, `contracts/inference-response.schema.json` | `response_validation.rs` |
| 012 runs and ordered events | `store/runs.rs`, `store/events.rs` | `store_runs.rs` |
| 013 streaming as events | `runtime.rs` (`RecordDeltas`), `dagos-openai/src/sse.rs`, `prose.rs` | `provider_fake.rs`, `runtime_fake_run.rs`, `mock_endpoint.rs` |
| 014 common provider interface | `provider.rs`, `provider/fake.rs`, `dagos-openai` | `provider_fake.rs`, `mock_endpoint.rs` |
| 015 provider/model identity | `domain/run.rs`, `store/runs.rs` | `runtime_config.rs` |
| 016 editable system prompt | `RunConfig`, `store/defaults.rs` | `runtime_config.rs` |
| 017 inspector | `dagos/src/inspect.rs`, `server.rs`, UI | `inspector.rs`, `cli.rs`, UI tests |
| 018 MCP optional | `dagos-mcp`, `workspace.rs` | `stdio.rs` (runs without MCP, failing servers skipped) |
| 019 invalid output fails closed | `response.rs`, `runtime.rs` | `response_validation.rs`, `hardening.rs`, `runtime_fake_run.rs` |
| 020 automated tests | all crates | the whole suite |
| 021 optional model Jev with fallback | `runtime.rs` (`with_jev_fallback`), `dagos/src/providers.rs` | `runtime_fake_run.rs`, `mock_endpoint.rs`, `providers.rs` tests |
| 022 keys in the app | `dagos/src/keys.rs`, `server.rs` | `keys.rs` tests, `settings_api.rs` |
| 023 conversations | `store/conversations.rs`, `runtime.rs` (`Thread`) | `conversations.rs`, `conversations_api.rs` |
| 024 Jev chooses tools | `runtime.rs` (`exposed_tools`), `protocol.rs` (tool questions) | `tools.rs`, `mock_endpoint.rs` |
| 025 recall and compaction | `context/recall.rs`, `runtime.rs` | `recall.rs`, `mock_endpoint.rs` |
| 026 review loop and lint | `runtime.rs` (`review`), `dagos-lint`, `dagos/src/main.rs` (`lint`) | `review.rs`, `dagos-lint` unit and `reviewer.rs`, `chat.test.mjs` |
| 027 tool checks only tighten | `dagos/src/approvals.rs`, `guard.rs` | `approvals.rs` and `guard.rs` tests, `tools.test.mjs` |
| tool execution (constitution principle 7) | `tools.rs`, `runtime.rs` (`run_tool`), `dagos-mcp`, `approvals.rs` | `tools.rs`, `tools_api.rs`, `stdio.rs` |

## Boundary checks (re-verified)
- **Jev only classifies.** Its three calls return node/tool labels (`classify`), per-chunk relevance
  (`relevance`), or per-question probabilities (`decide`); none returns text that is executed or
  stored. The runtime and the app act on the answers by fixed rules.
- **Providers receive compiled IR only.** `InferenceProvider::infer` takes `InferenceRequest { ir }`;
  `boundaries.rs` forbids the provider layer from depending on the store.
- **Validation precedes mutation.** Emissions are applied only from a `ValidatedResponse`, in one
  transaction with `response.validated`; tool output and review findings never mutate the DAG.
- **Loops are bounded.** At most 8 tool rounds and `max_rounds` (3) reviews per run; the review loop
  also stops when the code stops changing.
