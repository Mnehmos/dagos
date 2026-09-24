# Tasks: DAGOS Core Runtime

## Foundation
- [x] T001 Establish Rust project structure and module boundaries.
- [x] T002 Add SQLite bootstrap and migrations.
- [x] T003 Add serde JSON types and common IDs/timestamps.
- [x] T004 Add machine-readable contracts and validation tests.
- [x] T005 Add CI test, format, and lint checks.

## Durable DAG
- [x] T006 Implement DagNode and DagEdge types.
- [x] T007 Implement node persistence.
- [x] T008 Implement edge persistence.
- [x] T009 Implement Run and Event persistence.
- [x] T010 Implement ActiveContext persistence.
- [x] T011 Test DAG and active-context invariants transactionally.

## Jev
- [x] T012 Define Jev classification contract.
- [x] T013 Implement Jev adapter trait.
- [x] T014 Implement deterministic fake Jev.
- [x] T015 Apply classifications to active context.
- [x] T016 Reject output outside the classification contract.

## IR
- [x] T017 Define inference IR v1.
- [x] T018 Implement deterministic IR compiler.
- [x] T019 Add IR snapshot tests.
- [x] T020 Prove providers receive IR only.

## Provider Runtime
- [x] T021 Define provider adapter trait.
- [x] T022 Implement fake streaming provider.
- [x] T023 Persist inference delta events.
- [x] T024 Validate final structured responses.
- [x] T025 Apply valid emissions to DAG state.
- [x] T026 Test malformed responses, timeouts, and failure state.

## First Real Provider
- [x] T027 Implement one real provider behind the common interface.
- [x] T028 Keep provider configuration outside core domain types.
- [x] T029 Add opt-in live-provider integration coverage.

## Inspector and Release
- [x] T030 Implement read-only state inspection.
- [x] T031 Add restart/persistence tests.
- [x] T032 Verify MCP is optional.
- [x] T033 Run Spec Kit analyze (`checklists/traceability.md`).
- [x] T034 Run implement/converge cycle (plan, research, data model, quickstart, and scope reconciled).
- [ ] T035 Tag v0.1.

## Converged Scope
- [x] T036 App UI: chat, inspector, DAG view, settings, keyboard use, light and dark.
- [x] T037 Saved per-user keys, provider presets and custom endpoints.
- [x] T038 Optional model Jev with offline fallback; TypeSafe's Jev through the Decisions API.
- [x] T039 Conversations and projects (migration 0003).
- [x] T040 MCP tool execution: policies, approvals, step limit, timeouts, Claude Desktop import.
- [x] T041 Markdown replies and tool output; grouped tool calls.
- [x] T042 Jev chooses each run's tools.
- [x] T043 Automatic recall across chats and compaction of stale tool results.
- [x] T044 Semantic lint (`dagos-lint`, `dagos lint`) and the bounded review loop.
- [x] T045 Tool guard and meta-tool policies.

Dependency order: T001-T005 -> T006-T011 -> T012-T016 -> T017-T020 -> T021-T026 -> T027-T029 -> T030-T034 -> T036-T045 -> T035.