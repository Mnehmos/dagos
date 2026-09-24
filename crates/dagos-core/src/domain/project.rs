//! Projects: the scope that owns one DAG and its runs.

use serde::{Deserialize, Serialize};

use super::ids::{ConversationId, ProjectId};
use super::time::Timestamp;

/// A DAGOS project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub created_at: Timestamp,
}

/// A conversation: a thread of runs inside a project. The project's DAG is shared by all of its
/// conversations; a conversation carries active context from one of its runs to the next and
/// gives the IR its recent turns. Archived conversations are hidden, never deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conversation {
    pub id: ConversationId,
    pub project_id: ProjectId,
    pub title: String,
    pub created_at: Timestamp,
    /// When its latest run started (or when it was created).
    pub updated_at: Timestamp,
    pub archived_at: Option<Timestamp>,
}
