//! Run repository.
//!
//! The store keeps each run's lifecycle consistent with its event history: creating a run records
//! `run.started` as event 1, and completing or failing it records the terminal event in the same
//! transaction as the status change. Only `running` runs change state.

use rusqlite::{OptionalExtension, Row, params};

use super::{StoreError, Tx};
use crate::domain::{ErrorCode, EventData, ProjectId, Run, RunConfig, RunId, RunStatus, Timestamp};

const RUN_COLUMNS: &str = "id, project_id, provider_id, model_id, system_prompt, status, \
                           started_at, completed_at, error_code";

fn run_from_row(row: &Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: row.get(0)?,
        project_id: row.get(1)?,
        provider_id: row.get(2)?,
        model_id: row.get(3)?,
        system_prompt: row.get(4)?,
        status: row.get(5)?,
        started_at: row.get(6)?,
        completed_at: row.get(7)?,
        error_code: row.get(8)?,
    })
}

impl Tx<'_> {
    /// Starts a run in `project_id` with `config`, recording `run.started`.
    ///
    /// v0.1 runs one pipeline at a time per project: fails with [`StoreError::RunInProgress`]
    /// while another run is still running.
    pub fn create_run(
        &self,
        project_id: &ProjectId,
        config: &RunConfig,
    ) -> Result<Run, StoreError> {
        self.require_project(project_id)?;
        if let Some(running) = self.running_run(project_id)? {
            return Err(StoreError::RunInProgress { running: running.id });
        }
        let run = Run {
            id: RunId::generate(self.ids),
            project_id: project_id.clone(),
            provider_id: config.provider_id.clone(),
            model_id: config.model_id.clone(),
            system_prompt: config.system_prompt.clone(),
            status: RunStatus::Running,
            started_at: self.now(),
            completed_at: None,
            error_code: None,
        };
        self.conn.execute(
            &format!(
                "INSERT INTO runs ({RUN_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
            ),
            params![
                run.id,
                run.project_id,
                run.provider_id,
                run.model_id,
                run.system_prompt,
                run.status,
                run.started_at,
                run.completed_at,
                run.error_code
            ],
        )?;
        self.append_event(
            &run.id,
            EventData::RunStarted {
                provider_id: run.provider_id.clone(),
                model_id: run.model_id.clone(),
                system_prompt: run.system_prompt.clone(),
            },
        )?;
        Ok(run)
    }

    pub fn run(&self, id: &RunId) -> Result<Option<Run>, StoreError> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {RUN_COLUMNS} FROM runs WHERE id = ?1"), [id], run_from_row)
            .optional()?)
    }

    /// Every run of the project, oldest first.
    pub fn runs(&self, project_id: &ProjectId) -> Result<Vec<Run>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM runs WHERE project_id = ?1 ORDER BY started_at, id"
        ))?;
        let runs = statement.query_map([project_id], run_from_row)?.collect::<Result<_, _>>()?;
        Ok(runs)
    }

    /// The run of the same project that started immediately before `id`, if any.
    pub fn previous_run(&self, id: &RunId) -> Result<Option<Run>, StoreError> {
        let run = self.run(id)?.ok_or_else(|| Self::not_found("run", id))?;
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {RUN_COLUMNS} FROM runs
                     WHERE project_id = ?1 AND (started_at, id) < (?2, ?3)
                     ORDER BY started_at DESC, id DESC LIMIT 1"
                ),
                params![run.project_id, run.started_at, run.id],
                run_from_row,
            )
            .optional()?)
    }

    /// The project's running run, if any.
    pub fn running_run(&self, project_id: &ProjectId) -> Result<Option<Run>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {RUN_COLUMNS} FROM runs WHERE project_id = ?1 AND status = ?2"),
                params![project_id, RunStatus::Running],
                run_from_row,
            )
            .optional()?)
    }

    /// Every run still marked `running`, across projects (e.g. left behind by a crash).
    pub fn running_runs(&self) -> Result<Vec<Run>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM runs WHERE status = ?1 ORDER BY started_at, id"
        ))?;
        let runs =
            statement.query_map([RunStatus::Running], run_from_row)?.collect::<Result<_, _>>()?;
        Ok(runs)
    }

    /// Marks a running run completed and records `run.completed`.
    pub fn complete_run(&self, id: &RunId) -> Result<Run, StoreError> {
        self.append_event(id, EventData::RunCompleted {})?;
        self.finish_run(id, RunStatus::Completed, None)
    }

    /// Marks a running run failed with `error_code` and records `run.failed`. State committed
    /// earlier in the run is left intact.
    pub fn fail_run(
        &self,
        id: &RunId,
        error_code: ErrorCode,
        message: &str,
    ) -> Result<Run, StoreError> {
        self.append_event(id, EventData::RunFailed { error_code, message: message.to_owned() })?;
        self.finish_run(id, RunStatus::Failed, Some(error_code))
    }

    fn finish_run(
        &self,
        id: &RunId,
        status: RunStatus,
        error_code: Option<ErrorCode>,
    ) -> Result<Run, StoreError> {
        let completed_at: Timestamp = self.now();
        self.conn.execute(
            "UPDATE runs SET status = ?2, completed_at = ?3, error_code = ?4 WHERE id = ?1",
            params![id, status, completed_at, error_code],
        )?;
        Ok(self.run(id)?.expect("run exists after update"))
    }

    /// Fails unless `id` names a run that is still running.
    pub(super) fn require_running(&self, id: &RunId) -> Result<Run, StoreError> {
        let run = self.run(id)?.ok_or_else(|| Self::not_found("run", id))?;
        if run.status == RunStatus::Running {
            Ok(run)
        } else {
            Err(StoreError::RunFinished { id: run.id, status: run.status })
        }
    }
}
