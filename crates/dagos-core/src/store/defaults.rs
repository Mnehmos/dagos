//! Per-project defaults for new runs: provider, model, and the editable system prompt.

use rusqlite::{OptionalExtension, params};

use super::{StoreError, Tx};
use crate::domain::{ProjectId, RunConfig};

impl Tx<'_> {
    /// The project's defaults for new runs, if any have been set.
    pub fn run_defaults(&self, project_id: &ProjectId) -> Result<Option<RunConfig>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT provider_id, model_id, system_prompt FROM run_defaults WHERE project_id = ?1",
                [project_id],
                |row| {
                    Ok(RunConfig {
                        provider_id: row.get(0)?,
                        model_id: row.get(1)?,
                        system_prompt: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Sets the project's defaults for new runs. Runs already started keep the configuration they
    /// recorded when they started.
    pub fn set_run_defaults(
        &self,
        project_id: &ProjectId,
        config: &RunConfig,
    ) -> Result<(), StoreError> {
        self.require_project(project_id)?;
        self.conn.execute(
            "INSERT INTO run_defaults (project_id, provider_id, model_id, system_prompt, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (project_id) DO UPDATE SET
                 provider_id = excluded.provider_id,
                 model_id = excluded.model_id,
                 system_prompt = excluded.system_prompt,
                 updated_at = excluded.updated_at",
            params![
                project_id,
                config.provider_id,
                config.model_id,
                config.system_prompt,
                self.now()
            ],
        )?;
        Ok(())
    }
}
