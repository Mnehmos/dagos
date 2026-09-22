# Implementation Plan: DAGOS Core Runtime

Feature: 001-core-runtime

## Summary
Implement a local-first DAG runtime with SQLite persistence, Jev classifier adapter, versioned inference IR, provider adapter interface, structured response validation, event log, and a minimal runtime boundary.

Start with fake Jev and fake provider so the architecture is testable without external services.

## Technical Context
Language: Rust stable
Storage: SQLite
Testing: Rust unit and integration tests with temporary databases and fake adapters
Targets: Windows and Linux local development
Project type: local runtime/library
Constraints: local-first, provider-neutral, deterministic state transitions, no raw DAG crossing the provider boundary
v0.1 scale: one local project and one active run at a time

## Architecture
User Message -> Run -> Jev Classification -> Active Context Projection -> IR Compiler -> Provider Adapter -> Structured Response -> DAG Emissions -> Event Log

## Layers
1. domain: typed nodes, edges, runs, events, classifications, IR, responses
2. store: SQLite persistence and transactions
3. context: active-context projection and Jev adapter
4. ir: deterministic compilation to versioned IR
5. providers: common trait plus fake provider
6. runtime: run lifecycle
7. transport: minimal CLI/API boundary
8. tests: contract and integration tests

## Phases
Phase 0: Rust project, SQLite bootstrap, JSON contracts, IDs, timestamps, CI.
Phase 1: DAG node/edge persistence, runs/events, active context, transaction tests.
Phase 2: Jev request/response, adapter, fake classifier, classification application, strict boundary tests.
Phase 3: IR v1, deterministic compiler, snapshots, proof that providers receive IR only.
Phase 4: Provider trait, fake streaming provider, delta events, final response validation, emissions, failure tests.
Phase 5: One real provider behind the common interface.
Phase 6: Read-only inspector for DAG, context, run, events, provider/model, IR, response.
Phase 7: restart tests, MCP-optional verification, Spec Kit analyze, implement, converge, v0.1 tag.

## Constitution Gate
Jev remains classification-only. Providers remain interchangeable. Raw DAG never crosses the provider interface. Context removal preserves durable state. Machine interfaces remain versioned JSON. No autonomous agent loop is introduced.