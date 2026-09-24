-- DAGOS v0.1 initial schema.
--
-- Invariants encoded here (see specs/001-core-runtime/data-model.md):
-- * DAG nodes and edges are durable: they cannot be deleted.
-- * Active context references nodes; removing a context row never touches the node.
-- * Event history is append-only and sequences are unique per run.
-- * Every run carries provider and model identity; at most one run per project is running.

CREATE TABLE projects (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL CHECK (length(name) > 0),
    created_at  TEXT NOT NULL
) STRICT;

CREATE TABLE dag_nodes (
    id            TEXT PRIMARY KEY NOT NULL,
    project_id    TEXT NOT NULL REFERENCES projects (id),
    type          TEXT NOT NULL CHECK (type IN (
                      'task', 'artifact', 'observation', 'decision', 'result', 'conversation')),
    payload_json  TEXT NOT NULL CHECK (json_valid(payload_json) AND json_type(payload_json) = 'object'),
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    -- Target for the composite foreign keys that keep edges inside one project.
    UNIQUE (project_id, id)
) STRICT;

CREATE INDEX dag_nodes_by_project ON dag_nodes (project_id, created_at, id);

CREATE TABLE dag_edges (
    id            TEXT PRIMARY KEY NOT NULL,
    project_id    TEXT NOT NULL REFERENCES projects (id),
    from_node_id  TEXT NOT NULL,
    to_node_id    TEXT NOT NULL,
    type          TEXT NOT NULL CHECK (type IN (
                      'depends_on', 'produces', 'observed_from', 'related_to', 'supersedes')),
    created_at    TEXT NOT NULL,
    FOREIGN KEY (project_id, from_node_id) REFERENCES dag_nodes (project_id, id),
    FOREIGN KEY (project_id, to_node_id) REFERENCES dag_nodes (project_id, id),
    CHECK (from_node_id <> to_node_id),
    UNIQUE (from_node_id, to_node_id, type)
) STRICT;

CREATE INDEX dag_edges_by_project ON dag_edges (project_id, created_at, id);
CREATE INDEX dag_edges_by_target ON dag_edges (to_node_id);

CREATE TABLE runs (
    id             TEXT PRIMARY KEY NOT NULL,
    project_id     TEXT NOT NULL REFERENCES projects (id),
    provider_id    TEXT NOT NULL CHECK (length(provider_id) > 0),
    model_id       TEXT NOT NULL CHECK (length(model_id) > 0),
    system_prompt  TEXT NOT NULL,
    status         TEXT NOT NULL CHECK (status IN ('running', 'completed', 'failed')),
    started_at     TEXT NOT NULL,
    completed_at   TEXT,
    error_code     TEXT,
    CHECK ((status = 'running') = (completed_at IS NULL)),
    CHECK ((status = 'failed') = (error_code IS NOT NULL))
) STRICT;

CREATE INDEX runs_by_project ON runs (project_id, started_at, id);
CREATE UNIQUE INDEX runs_one_running_per_project ON runs (project_id) WHERE status = 'running';

CREATE TABLE events (
    id            TEXT PRIMARY KEY NOT NULL,
    run_id        TEXT NOT NULL REFERENCES runs (id),
    sequence      INTEGER NOT NULL CHECK (sequence >= 1),
    type          TEXT NOT NULL CHECK (length(type) > 0),
    payload_json  TEXT NOT NULL CHECK (json_valid(payload_json) AND json_type(payload_json) = 'object'),
    created_at    TEXT NOT NULL,
    UNIQUE (run_id, sequence)
) STRICT;

-- One row per node that is a member of a run's active context. The row only references the node;
-- deleting it (removal from context) never affects the durable DAG.
CREATE TABLE active_context (
    run_id          TEXT NOT NULL REFERENCES runs (id),
    node_id         TEXT NOT NULL REFERENCES dag_nodes (id),
    classification  TEXT NOT NULL CHECK (classification = 'active'),
    ordering        INTEGER NOT NULL CHECK (ordering >= 0),
    source          TEXT NOT NULL CHECK (source IN ('carried', 'jev')),
    PRIMARY KEY (run_id, node_id),
    UNIQUE (run_id, ordering)
) STRICT;

CREATE TRIGGER dag_nodes_are_durable BEFORE DELETE ON dag_nodes
BEGIN
    SELECT RAISE(ABORT, 'dag nodes are durable and cannot be deleted');
END;

CREATE TRIGGER dag_edges_are_durable BEFORE DELETE ON dag_edges
BEGIN
    SELECT RAISE(ABORT, 'dag edges are durable and cannot be deleted');
END;

CREATE TRIGGER events_are_append_only_update BEFORE UPDATE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;

CREATE TRIGGER events_are_append_only_delete BEFORE DELETE ON events
BEGIN
    SELECT RAISE(ABORT, 'events are append-only');
END;
