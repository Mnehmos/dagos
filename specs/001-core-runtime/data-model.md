# Data Model

DagNode: id, type, payload_json, created_at, updated_at.
DagEdge: id, from_node_id, to_node_id, type, created_at.
ActiveContext: run_id, node_id, classification, ordering, source.
Run: id, project_id, provider_id, model_id, system_prompt, status, started_at, completed_at, error_code.
Event: id, run_id, sequence, type, payload_json, created_at.

Invariants:
1. DAG nodes outlive active-context membership.
2. Removing an active-context row does not delete a node.
3. Event sequence numbers are monotonic per run.
4. Provider and model identity are preserved.
5. Final responses are validated before semantic emissions mutate canonical state.