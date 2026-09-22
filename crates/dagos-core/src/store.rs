//! Store layer: SQLite persistence for projects, DAG nodes and edges, runs, events, and active
//! context.
//!
//! All persistence concerns live here. Active-context rows are stored independently of DAG nodes:
//! removing a context row never deletes a node.
