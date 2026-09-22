//! Active-context repository: each run's projection of the durable DAG.
//!
//! Membership rows reference nodes and nothing else, so removing a member never touches the node.
//! Membership is written as a whole with [`Tx::replace_context`], which assigns `ordering` by node
//! creation order (`created_at`, then `id`): the same member set always yields the same order.
//! Context is runtime state of a running run; once a run finishes, its context is frozen history.

use std::collections::BTreeSet;

use rusqlite::{Row, params};

use super::{DagViolation, StoreError, Tx};
use crate::domain::{Classification, ContextMember, ContextSource, NodeId, RunId};

fn member_from_row(row: &Row<'_>) -> rusqlite::Result<ContextMember> {
    Ok(ContextMember {
        run_id: row.get(0)?,
        node_id: row.get(1)?,
        classification: row.get(2)?,
        ordering: row.get(3)?,
        source: row.get(4)?,
    })
}

impl Tx<'_> {
    /// The run's active context in IR order.
    pub fn context(&self, run_id: &RunId) -> Result<Vec<ContextMember>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT run_id, node_id, classification, ordering, source
             FROM active_context WHERE run_id = ?1 ORDER BY ordering",
        )?;
        let members = statement.query_map([run_id], member_from_row)?.collect::<Result<_, _>>()?;
        Ok(members)
    }

    /// Replaces a running run's active context with `members`.
    ///
    /// Every node must belong to the run's project and appear at most once. Nodes left out lose
    /// their membership; the nodes themselves are untouched.
    pub fn replace_context(
        &self,
        run_id: &RunId,
        members: &[(NodeId, ContextSource)],
    ) -> Result<Vec<ContextMember>, StoreError> {
        let run = self.require_running(run_id)?;
        let mut seen = BTreeSet::new();
        let mut ranked = Vec::with_capacity(members.len());
        for (node_id, source) in members {
            if !seen.insert(node_id) {
                return Err(StoreError::DuplicateContextMember(node_id.clone()));
            }
            if !self.node_in_project(&run.project_id, node_id)? {
                return Err(DagViolation::UnknownNode(node_id.clone()).into());
            }
            let node = self.node(node_id)?.expect("node exists in the run's project");
            ranked.push((node.created_at, node.id, *source));
        }
        ranked.sort();

        self.conn.execute("DELETE FROM active_context WHERE run_id = ?1", [run_id])?;
        for (ordering, (_, node_id, source)) in ranked.into_iter().enumerate() {
            self.conn.execute(
                "INSERT INTO active_context (run_id, node_id, classification, ordering, source)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![run_id, node_id, Classification::Active, ordering as u32, source],
            )?;
        }
        self.context(run_id)
    }
}
