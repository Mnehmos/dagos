//! Project repository.

use rusqlite::{OptionalExtension, Row, params};

use super::{StoreError, Tx};
use crate::domain::{Project, ProjectId};

fn project_from_row(row: &Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project { id: row.get(0)?, name: row.get(1)?, created_at: row.get(2)? })
}

impl Tx<'_> {
    /// Creates a project. The name must not be empty.
    pub fn create_project(&self, name: &str) -> Result<Project, StoreError> {
        let project = Project {
            id: ProjectId::generate(self.ids),
            name: name.to_owned(),
            created_at: self.now(),
        };
        self.conn.execute(
            "INSERT INTO projects (id, name, created_at) VALUES (?1, ?2, ?3)",
            params![project.id, project.name, project.created_at],
        )?;
        Ok(project)
    }

    pub fn project(&self, id: &ProjectId) -> Result<Option<Project>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, name, created_at FROM projects WHERE id = ?1",
                [id],
                project_from_row,
            )
            .optional()?)
    }

    /// Renames a project; the name must not be blank.
    pub fn rename_project(&self, id: &ProjectId, name: &str) -> Result<Project, StoreError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(StoreError::Invalid("the project name is empty"));
        }
        self.require_project(id)?;
        self.conn.execute("UPDATE projects SET name = ?2 WHERE id = ?1", params![id, name])?;
        Ok(self.project(id)?.expect("project exists after update"))
    }

    /// All projects, oldest first.
    pub fn projects(&self) -> Result<Vec<Project>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT id, name, created_at FROM projects ORDER BY created_at, id")?;
        let projects = statement.query_map([], project_from_row)?.collect::<Result<_, _>>()?;
        Ok(projects)
    }
}
