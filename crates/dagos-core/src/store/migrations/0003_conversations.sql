-- Conversations: threads of runs inside a project. The project's DAG stays the shared, durable
-- memory; a conversation only groups runs, carries active context from one of its runs to the
-- next, and gives the IR its recent turns. Conversations are renamed or archived, never deleted,
-- because their runs and events are durable history.

CREATE TABLE conversations (
    id           TEXT PRIMARY KEY NOT NULL,
    project_id   TEXT NOT NULL REFERENCES projects (id),
    title        TEXT NOT NULL CHECK (length(title) > 0),
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    archived_at  TEXT
) STRICT;

CREATE INDEX conversations_by_project ON conversations (project_id, updated_at, id);

ALTER TABLE runs ADD COLUMN conversation_id TEXT REFERENCES conversations (id);

CREATE INDEX runs_by_conversation ON runs (conversation_id, started_at, id);

-- Runs recorded before conversations existed form one conversation per project, titled after its
-- first message.
INSERT INTO conversations (id, project_id, title, created_at, updated_at)
SELECT 'conv_' || lower(hex(randomblob(16))),
       runs.project_id,
       coalesce(
           nullif(trim(substr((
               SELECT json_extract(events.payload_json, '$.text')
               FROM runs AS first
               JOIN events ON events.run_id = first.id AND events.type = 'message.recorded'
               WHERE first.project_id = runs.project_id
               ORDER BY first.started_at, first.id
               LIMIT 1
           ), 1, 80)), ''),
           'Earlier runs'
       ),
       min(runs.started_at),
       max(runs.started_at)
FROM runs
GROUP BY runs.project_id;

UPDATE runs
SET conversation_id = (
    SELECT conversations.id FROM conversations WHERE conversations.project_id = runs.project_id
);
