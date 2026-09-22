# Data Model

Project: id, name, created_at.
DagNode: id, project_id, type, payload_json, created_at, updated_at.
DagEdge: id, project_id, from_node_id, to_node_id, type, created_at.
ActiveContext: run_id, node_id, classification, ordering, source.
Run: id, project_id, provider_id, model_id, system_prompt, status, started_at, completed_at, error_code.
Event: id, run_id, sequence, type, payload_json, created_at.

`project_id` is a storage scope: the node contract (`contracts/dag.schema.json`) does not carry it.

Invariants:
1. DAG nodes outlive active-context membership.
2. Removing an active-context row does not delete a node.
3. Event sequence numbers are monotonic per run.
4. Provider and model identity are preserved.
5. Final responses are validated before semantic emissions mutate canonical state.

Enforced by the SQLite schema (`crates/dagos-core/src/store/migrations`):
- DAG nodes and edges cannot be deleted; events cannot be updated or deleted.
- Edges connect two nodes of the same project, never a node to itself, and are unique per (from, to, type).
- Node payloads are JSON objects; node and edge types are the closed v1 enums.
- A run's status agrees with `completed_at` and `error_code`; at most one run per project is running.
- Active-context rows reference nodes (never the reverse), have unique ordering per run, and exist
  only for `active` classifications.
- Schema versions are tracked in `PRAGMA user_version`; migrations are append-only and transactional.
