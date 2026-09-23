//! Conversation repository.

use rusqlite::{OptionalExtension, Row, params};

use super::{StoreError, Tx};
use crate::domain::{Conversation, ConversationId, ProjectId};

/// The longest conversation title kept; longer titles are shortened.
pub const MAX_TITLE_CHARS: usize = 80;

const COLUMNS: &str = "id, project_id, title, created_at, updated_at, archived_at";

fn conversation_from_row(row: &Row<'_>) -> rusqlite::Result<Conversation> {
    Ok(Conversation {
        id: row.get(0)?,
        project_id: row.get(1)?,
        title: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
        archived_at: row.get(5)?,
    })
}

/// `title` on one line, trimmed and shortened to [`MAX_TITLE_CHARS`]; `None` if it is blank.
pub fn clean_title(title: &str) -> Option<String> {
    let line = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return None;
    }
    let mut chars = line.chars();
    let short: String = chars.by_ref().take(MAX_TITLE_CHARS).collect();
    Some(if chars.next().is_some() { format!("{}…", short.trim_end()) } else { short })
}

impl Tx<'_> {
    /// Starts a conversation in `project_id`. The title is cleaned with [`clean_title`].
    pub fn create_conversation(
        &self,
        project_id: &ProjectId,
        title: &str,
    ) -> Result<Conversation, StoreError> {
        self.require_project(project_id)?;
        let now = self.now();
        let conversation = Conversation {
            id: ConversationId::generate(self.ids),
            project_id: project_id.clone(),
            title: clean_title(title).unwrap_or_else(|| "New conversation".to_owned()),
            created_at: now,
            updated_at: now,
            archived_at: None,
        };
        self.conn.execute(
            &format!("INSERT INTO conversations ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"),
            params![
                conversation.id,
                conversation.project_id,
                conversation.title,
                conversation.created_at,
                conversation.updated_at,
                conversation.archived_at
            ],
        )?;
        Ok(conversation)
    }

    pub fn conversation(&self, id: &ConversationId) -> Result<Option<Conversation>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM conversations WHERE id = ?1"),
                [id],
                conversation_from_row,
            )
            .optional()?)
    }

    /// The project's conversations, most recently active first (archived ones included).
    pub fn conversations(&self, project_id: &ProjectId) -> Result<Vec<Conversation>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM conversations WHERE project_id = ?1
             ORDER BY updated_at DESC, id DESC"
        ))?;
        let conversations =
            statement.query_map([project_id], conversation_from_row)?.collect::<Result<_, _>>()?;
        Ok(conversations)
    }

    /// The project's most recently active conversation that is not archived.
    pub fn latest_conversation(
        &self,
        project_id: &ProjectId,
    ) -> Result<Option<Conversation>, StoreError> {
        Ok(self.conversations(project_id)?.into_iter().find(|c| c.archived_at.is_none()))
    }

    /// Renames a conversation; the title is cleaned with [`clean_title`] and must not be blank.
    pub fn rename_conversation(
        &self,
        id: &ConversationId,
        title: &str,
    ) -> Result<Conversation, StoreError> {
        let title = clean_title(title).ok_or(StoreError::Invalid("the title is empty"))?;
        self.require_conversation(id)?;
        self.conn
            .execute("UPDATE conversations SET title = ?2 WHERE id = ?1", params![id, title])?;
        self.require_conversation(id)
    }

    /// Archives (hides) or restores a conversation. Its runs stay durable either way.
    pub fn set_conversation_archived(
        &self,
        id: &ConversationId,
        archived: bool,
    ) -> Result<Conversation, StoreError> {
        let current = self.require_conversation(id)?;
        let archived_at = match (archived, current.archived_at) {
            (true, Some(at)) => Some(at),
            (true, None) => Some(self.now()),
            (false, _) => None,
        };
        self.conn.execute(
            "UPDATE conversations SET archived_at = ?2 WHERE id = ?1",
            params![id, archived_at],
        )?;
        self.require_conversation(id)
    }

    /// Marks a conversation active now (a run started in it) and restores it if archived.
    pub(super) fn touch_conversation(&self, id: &ConversationId) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE conversations SET updated_at = ?2, archived_at = NULL WHERE id = ?1",
            params![id, self.now()],
        )?;
        Ok(())
    }

    pub(super) fn require_conversation(
        &self,
        id: &ConversationId,
    ) -> Result<Conversation, StoreError> {
        self.conversation(id)?.ok_or_else(|| Self::not_found("conversation", id))
    }
}
