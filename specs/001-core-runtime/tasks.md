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
- [ ] T015 Apply classifications to active context.
- [ ] T016 Reject output outside the classification contract.

## IR
- [ ] T017 Define inference IR v1.
- [ ] T018 Implement deterministic IR compiler.
- [ ] T019 Add IR snapshot tests.
- [ ] T020 Prove providers receive IR only.

## Provider Runtime
- [ ] T021 Define provider adapter trait.
- [ ] T022 Implement fake streaming provider.
- [ ] T023 Persist inference delta events.
- [ ] T024 Validate final structured responses.
- [ ] T025 Apply valid emissions to DAG state.
- [ ] T026 Test malformed responses, timeouts, and failure state.

## First Real Provider
- [ ] T027 Implement one real provider behind the common interface.
- [ ] T028 Keep provider configuration outside core domain types.
- [ ] T029 Add opt-in live-provider integration coverage.

## Inspector and Release
- [ ] T030 Implement read-only state inspection.
- [ ] T031 Add restart/persistence tests.
- [ ] T032 Verify MCP is optional.
- [ ] T033 Run Spec Kit analyze.
- [ ] T034 Run implement/converge cycle.
- [ ] T035 Tag v0.1.

Dependency order: T001-T005 -> T006-T011 -> T012-T016 -> T017-T020 -> T021-T026 -> T027-T029 -> T030-T035.