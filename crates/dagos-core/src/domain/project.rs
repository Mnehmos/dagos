//! Projects: the scope that owns one DAG and its runs.

use serde::{Deserialize, Serialize};

use super::ids::ProjectId;
use super::time::Timestamp;

/// A DAGOS project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub created_at: Timestamp,
}
