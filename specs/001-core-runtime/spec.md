# Feature Specification: DAGOS Core Runtime

Feature: 001-core-runtime

## Purpose
Build the smallest usable DAGOS runtime that accepts a coding request, maintains durable project state, classifies active context with Jev, compiles a versioned IR, invokes a selected inference provider, and persists structured results.

## User Stories
US1: A developer submits a coding message and receives streamed output through a configured provider.
US2: Requests, observations, decisions, artifacts, and conversation turns persist after the run.
US3: Jev classifies durable nodes for active or removed context without deleting them.
US4: A provider and model can be selected without changing DAGOS state semantics.
US5: A developer can inspect DAG state, active context, events, IR, and structured response.

## Functional Requirements
FR-001 Persist DAG nodes and edges.
FR-002 Support node types task, artifact, observation, decision, result, conversation.
FR-003 Support edges depends_on, produces, observed_from, related_to, supersedes.
FR-004 Maintain active-context membership separately from DAG storage.
FR-005 Removing active context never deletes the DAG node.
FR-006 Jev accepts a machine-readable classification request.
FR-007 Jev output is limited to context classification.
FR-008 Compile active state into a versioned inference IR.
FR-009 Providers receive IR, not raw DAG records.
FR-010 Provider input is JSON.
FR-011 Provider output is JSON with explicit presentation prose and structured emissions.
FR-012 Persist runs and ordered events.
FR-013 Support inference streaming as ordered events.
FR-014 Provider adapters implement a common interface.
FR-015 Persist provider and model identity on every run.
FR-016 Represent the editable system prompt explicitly.
FR-017 Expose enough state for a basic inspector.
FR-018 MCP is optional and not required for a basic run.
FR-019 Invalid provider responses fail closed and create an error event.
FR-020 Core state transitions have automated tests.
FR-021 A model-backed Jev is optional: when it fails or its output is rejected, the run records why and the offline classifier classifies the same request.
FR-022 Provider API keys can be managed in the app; saved keys live per user outside the project and never appear in the database, the IR, or API responses.
FR-023 Runs belong to conversations inside a project: a conversation carries active context between its runs and gives the IR its recent turns (request and reply); all conversations of a project share its durable DAG. Conversations are renamed or archived, never deleted.
FR-024 Jev may also classify tool candidates active or inactive per run; only tools it does not mark inactive are compiled into the IR. Tool permission remains with the tool policy and the person.
FR-025 The IR carries only a conversation's most recent turns (8 by default). Older turns are never summarized away: while a conversation has any, the run offers the built-in `dagos.recall` tool, which returns the earlier turns relevant to a query verbatim (each turn is one chunk, judged relevant or not by Jev, or by word overlap without a model Jev) with `before` / `after` cursors to neighbouring turns. Recall reads only the run's own conversation, so it needs no approval.

## Acceptance Scenarios
1. Empty project + user message creates conversation state and a run.
2. Jev classification changes active context without deleting durable nodes.
3. Inference receives only versioned IR.
4. Valid structured emissions become durable DAG records.
5. Malformed provider output creates an error and no invalid semantic mutation.
6. A normal run works when MCP is unavailable.
7. A normal run works when the configured model Jev is unavailable or misbehaves.
8. A new conversation starts with empty active context yet can draw on the project's durable DAG; a continued conversation carries its context and recent turns.

## Out of Scope
Planning agents, autonomous loops, tool orchestration, provider routing, embeddings, vector databases, and complex memory policies.