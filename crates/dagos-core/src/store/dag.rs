//! Durable DAG repository: nodes and edges.
//!
//! Nodes and edges are never deleted. Edges are validated before insertion: both endpoints must
//! exist in the edge's project, an edge never connects a node to itself, `(from, to, type)` is
//! unique, and no edge may close a cycle — the graph stays acyclic.

use rusqlite::{OptionalExtension, Row, params};

use super::sql::Json;
use super::{DagViolation, StoreError, Tx};
use crate::domain::{DagEdge, DagNode, EdgeId, EdgeType, NodeId, NodeType, Payload, ProjectId};

const NODE_COLUMNS: &str = "id, type, payload_json, created_at, updated_at";
const EDGE_COLUMNS: &str = "id, from_node_id, to_node_id, type, created_at";

fn node_from_row(row: &Row<'_>) -> rusqlite::Result<DagNode> {
    Ok(DagNode {
        id: row.get(0)?,
        node_type: row.get(1)?,
        payload: row.get::<_, Json<Payload>>(2)?.0,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

fn edge_from_row(row: &Row<'_>) -> rusqlite::Result<DagEdge> {
    Ok(DagEdge {
        id: row.get(0)?,
        from_node_id: row.get(1)?,
        to_node_id: row.get(2)?,
        edge_type: row.get(3)?,
        created_at: row.get(4)?,
    })
}

impl Tx<'_> {
    /// Creates a durable node in `project_id`.
    pub fn insert_node(
        &self,
        project_id: &ProjectId,
        node_type: NodeType,
        payload: Payload,
    ) -> Result<DagNode, StoreError> {
        self.require_project(project_id)?;
        let now = self.now();
        let node = DagNode {
            id: NodeId::generate(self.ids),
            node_type,
            payload,
            created_at: now,
            updated_at: now,
        };
        self.conn.execute(
            "INSERT INTO dag_nodes (id, project_id, type, payload_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                node.id,
                project_id,
                node.node_type,
                Json(&node.payload),
                node.created_at,
                node.updated_at
            ],
        )?;
        Ok(node)
    }

    pub fn node(&self, id: &NodeId) -> Result<Option<DagNode>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {NODE_COLUMNS} FROM dag_nodes WHERE id = ?1"),
                [id],
                node_from_row,
            )
            .optional()?)
    }

    /// Every node of the project in creation order (`created_at`, then `id`).
    pub fn nodes(&self, project_id: &ProjectId) -> Result<Vec<DagNode>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {NODE_COLUMNS} FROM dag_nodes WHERE project_id = ?1 ORDER BY created_at, id"
        ))?;
        let nodes = statement.query_map([project_id], node_from_row)?.collect::<Result<_, _>>()?;
        Ok(nodes)
    }

    /// Replaces a node's payload and advances `updated_at`. Identity, type, and `created_at` never
    /// change.
    pub fn update_node_payload(
        &self,
        id: &NodeId,
        payload: Payload,
    ) -> Result<DagNode, StoreError> {
        let updated = self.conn.execute(
            "UPDATE dag_nodes SET payload_json = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, Json(&payload), self.now()],
        )?;
        if updated == 0 {
            return Err(Self::not_found("node", id));
        }
        Ok(self.node(id)?.expect("node exists after update"))
    }

    /// Creates a durable edge `from → to` after validating it against the current DAG.
    pub fn insert_edge(
        &self,
        project_id: &ProjectId,
        from: &NodeId,
        to: &NodeId,
        edge_type: EdgeType,
    ) -> Result<DagEdge, StoreError> {
        self.require_project(project_id)?;
        self.validate_edge(project_id, from, to, edge_type)?;
        let edge = DagEdge {
            id: EdgeId::generate(self.ids),
            from_node_id: from.clone(),
            to_node_id: to.clone(),
            edge_type,
            created_at: self.now(),
        };
        self.conn.execute(
            "INSERT INTO dag_edges (id, project_id, from_node_id, to_node_id, type, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                edge.id,
                project_id,
                edge.from_node_id,
                edge.to_node_id,
                edge.edge_type,
                edge.created_at
            ],
        )?;
        Ok(edge)
    }

    /// Rejects `from → to` with a [`DagViolation`] if inserting it would break a DAG invariant.
    fn validate_edge(
        &self,
        project_id: &ProjectId,
        from: &NodeId,
        to: &NodeId,
        edge_type: EdgeType,
    ) -> Result<(), StoreError> {
        for endpoint in [from, to] {
            if !self.node_in_project(project_id, endpoint)? {
                return Err(DagViolation::UnknownNode(endpoint.clone()).into());
            }
        }
        if from == to {
            return Err(DagViolation::SelfLoop(from.clone()).into());
        }
        let duplicate: bool = self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM dag_edges
                            WHERE from_node_id = ?1 AND to_node_id = ?2 AND type = ?3)",
            params![from, to, edge_type],
            |row| row.get(0),
        )?;
        if duplicate {
            return Err(DagViolation::DuplicateEdge {
                from: from.clone(),
                to: to.clone(),
                edge_type,
            }
            .into());
        }
        if self.reaches(to, from)? {
            return Err(
                DagViolation::Cycle { from: from.clone(), to: to.clone(), edge_type }.into()
            );
        }
        Ok(())
    }

    pub fn edge(&self, id: &EdgeId) -> Result<Option<DagEdge>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {EDGE_COLUMNS} FROM dag_edges WHERE id = ?1"),
                [id],
                edge_from_row,
            )
            .optional()?)
    }

    /// Every edge of the project in creation order (`created_at`, then `id`).
    pub fn edges(&self, project_id: &ProjectId) -> Result<Vec<DagEdge>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {EDGE_COLUMNS} FROM dag_edges WHERE project_id = ?1 ORDER BY created_at, id"
        ))?;
        let edges = statement.query_map([project_id], edge_from_row)?.collect::<Result<_, _>>()?;
        Ok(edges)
    }

    fn node_in_project(&self, project_id: &ProjectId, id: &NodeId) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM dag_nodes WHERE id = ?1 AND project_id = ?2)",
            params![id, project_id],
            |row| row.get(0),
        )?)
    }

    /// Whether a directed path leads from `start` to `target`.
    fn reaches(&self, start: &NodeId, target: &NodeId) -> Result<bool, StoreError> {
        Ok(self.conn.query_row(
            "WITH RECURSIVE reachable(id) AS (
                 SELECT ?1
                 UNION
                 SELECT dag_edges.to_node_id
                 FROM dag_edges JOIN reachable ON dag_edges.from_node_id = reachable.id
             )
             SELECT EXISTS (SELECT 1 FROM reachable WHERE id = ?2)",
            params![start, target],
            |row| row.get(0),
        )?)
    }
}
