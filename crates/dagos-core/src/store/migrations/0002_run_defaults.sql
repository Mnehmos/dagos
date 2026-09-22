-- Per-project defaults for new runs: the provider and model to use and the editable system prompt.
-- Each run still records the configuration it actually ran with; changing the defaults only
-- affects runs started afterwards.

CREATE TABLE run_defaults (
    project_id     TEXT PRIMARY KEY NOT NULL REFERENCES projects (id),
    provider_id    TEXT NOT NULL CHECK (length(provider_id) > 0),
    model_id       TEXT NOT NULL CHECK (length(model_id) > 0),
    system_prompt  TEXT NOT NULL,
    updated_at     TEXT NOT NULL
) STRICT;
