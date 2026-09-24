# Data Model

Project: id, name, created_at.
RunDefaults: project_id, provider_id, model_id, system_prompt, updated_at.
Conversation: id, project_id, title, created_at, updated_at, archived_at.
DagNode: id, project_id, type, payload_json, created_at, updated_at.
DagEdge: id, project_id, from_node_id, to_node_id, type, created_at.
ActiveContext: run_id, node_id, classification, ordering, source.
Run: id, project_id, conversation_id, provider_id, model_id, system_prompt, status, started_at, completed_at, error_code.
Event: id, run_id, sequence, type, payload_json, created_at.

`project_id` is a storage scope: the node contract (`contracts/dag.schema.json`) does not carry it.

Invariants:
1. DAG nodes outlive active-context membership.
2. Removing an active-context row does not delete a node.
3. Event sequence numbers are monotonic per run.
4. Provider and model identity are preserved.
5. Final responses are validated before semantic emissions mutate canonical state.
6. A conversation groups runs of one project: active context carries from a conversation's previous
   run, and the IR's recent turns come from the same conversation. The DAG belongs to the project
   and is shared by all of its conversations. Conversations are renamed or archived, never deleted.

Enforced by the SQLite schema (`crates/dagos-core/src/store/migrations`):
- DAG nodes and edges cannot be deleted; events cannot be updated or deleted.
- Edges connect two nodes of the same project, never a node to itself, and are unique per (from, to, type).
- Node payloads are JSON objects; node and edge types are the closed v1 enums.
- A run's status agrees with `completed_at` and `error_code`; at most one run per project is running.
- Active-context rows reference nodes (never the reverse), have unique ordering per run, and exist
  only for `active` classifications.
- Schema versions are tracked in `PRAGMA user_version`; migrations are append-only and transactional.

Event types (the closed v1 set, in the order a run can produce them):
`run.started`, `message.recorded`, `context.carried`, `jev.requested`, `jev.classified`,
`jev.rejected`, `jev.fallback`, `context.added`, `context.removed`, `ir.compiled`,
`inference.started`, `inference.delta`, `inference.completed`, `tool.requested`, `tool.decided`,
`tool.completed`, `review.completed`, `response.validated`, `response.rejected`, `dag.node_created`,
`dag.edge_created`, `run.completed`, `run.failed`. Tool output and review findings live only in
events; they never become DAG state by themselves.

What the IR carries (`contracts/inference-ir.schema.json`), and where each part comes from:
- `task`, `context`: the run's message node and active context (DAG, through Jev's classification).
- `recent_events`: the conversation's latest turns (8 by default), from events.
- `recalled`: older turns of any chat of the project that Jev judged relevant, verbatim, from events.
- `tools`: the offered tools Jev did not classify inactive for this run.
- `tool_results`: this run's calls and outcomes; large earlier results Jev judged not needed for
  the current step are replaced by an `omitted` note (the full output stays in `tool.completed`).
- `review`: the latest review's findings while the model must address them.

Workspace files (in the DAGOS directory unless noted), none of them in the database:
- `dagos.sqlite3`: the store above. Migrations: `0001_initial`, `0002_run_defaults`,
  `0003_conversations`.
- `providers.json`: custom endpoints and the model Jev choice.
- `mcp.json`: MCP servers and per-tool policies (`off`, `ask`, `allow`).
- `lint.json`: lint rules, threshold, review limit, and whether the review loop is on.
- Saved API keys live per user outside the project (`%APPDATA%/dagos/keys.json`,
  `~/.config/dagos/keys.json`), never in the database, IR, or API responses.
