//! Schema-level invariant tests. These use raw SQL on purpose: the constraints must hold even for
//! code that bypasses the repository API.

use rusqlite::Connection;

use super::*;

const SEED: &str = "
    INSERT INTO projects (id, name, created_at) VALUES
        ('proj_a', 'a', 't0'),
        ('proj_b', 'b', 't0');
    INSERT INTO dag_nodes (id, project_id, type, payload_json, created_at, updated_at) VALUES
        ('node_a1', 'proj_a', 'task', '{}', 't1', 't1'),
        ('node_a2', 'proj_a', 'decision', '{\"text\":\"x\"}', 't2', 't2'),
        ('node_b1', 'proj_b', 'task', '{}', 't1', 't1');
    INSERT INTO runs (id, project_id, provider_id, model_id, system_prompt, status, started_at) VALUES
        ('run_1', 'proj_a', 'fake', 'fake-echo', 'prompt', 'running', 't3');
";

fn seeded() -> Store {
    let store = Store::open_in_memory().unwrap();
    store.lock().execute_batch(SEED).unwrap();
    store
}

fn rejects(conn: &Connection, sql: &str) -> String {
    match conn.execute_batch(sql) {
        Ok(()) => panic!("expected constraint violation for: {sql}"),
        Err(error) => error.to_string(),
    }
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

#[test]
fn opens_a_fresh_file_database_and_reopens_it_with_data_intact() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dagos.sqlite3");
    {
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        store.lock().execute_batch(SEED).unwrap();
    }
    let reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), SCHEMA_VERSION);
    assert_eq!(count(&reopened.lock(), "SELECT count(*) FROM dag_nodes"), 3);
}

#[test]
fn foreign_keys_are_enforced() {
    let store = seeded();
    let conn = store.lock();
    let error = rejects(
        &conn,
        "INSERT INTO dag_edges VALUES ('edge_1', 'proj_a', 'node_a1', 'node_missing', 'depends_on', 't')",
    );
    assert!(error.contains("FOREIGN KEY"), "{error}");
}

#[test]
fn edges_cannot_cross_projects() {
    let store = seeded();
    let conn = store.lock();
    let error = rejects(
        &conn,
        "INSERT INTO dag_edges VALUES ('edge_1', 'proj_a', 'node_a1', 'node_b1', 'related_to', 't')",
    );
    assert!(error.contains("FOREIGN KEY"), "{error}");
}

#[test]
fn edges_reject_self_loops_duplicates_and_unknown_types() {
    let store = seeded();
    let conn = store.lock();
    rejects(
        &conn,
        "INSERT INTO dag_edges VALUES ('edge_1', 'proj_a', 'node_a1', 'node_a1', 'depends_on', 't')",
    );
    rejects(
        &conn,
        "INSERT INTO dag_edges VALUES ('edge_1', 'proj_a', 'node_a1', 'node_a2', 'blocks', 't')",
    );
    conn.execute_batch(
        "INSERT INTO dag_edges VALUES ('edge_1', 'proj_a', 'node_a1', 'node_a2', 'depends_on', 't')",
    )
    .unwrap();
    rejects(
        &conn,
        "INSERT INTO dag_edges VALUES ('edge_2', 'proj_a', 'node_a1', 'node_a2', 'depends_on', 't')",
    );
}

#[test]
fn nodes_require_a_known_type_and_a_json_object_payload() {
    let store = seeded();
    let conn = store.lock();
    rejects(&conn, "INSERT INTO dag_nodes VALUES ('node_x', 'proj_a', 'plan', '{}', 't', 't')");
    rejects(&conn, "INSERT INTO dag_nodes VALUES ('node_x', 'proj_a', 'task', '[1,2]', 't', 't')");
    rejects(
        &conn,
        "INSERT INTO dag_nodes VALUES ('node_x', 'proj_a', 'task', 'not json', 't', 't')",
    );
}

#[test]
fn dag_nodes_and_edges_cannot_be_deleted() {
    let store = seeded();
    let conn = store.lock();
    conn.execute_batch(
        "INSERT INTO dag_edges VALUES ('edge_1', 'proj_a', 'node_a1', 'node_a2', 'depends_on', 't')",
    )
    .unwrap();
    let error = rejects(&conn, "DELETE FROM dag_nodes WHERE id = 'node_b1'");
    assert!(error.contains("durable"), "{error}");
    let error = rejects(&conn, "DELETE FROM dag_edges WHERE id = 'edge_1'");
    assert!(error.contains("durable"), "{error}");
}

#[test]
fn removing_an_active_context_row_never_deletes_the_node() {
    let store = seeded();
    let conn = store.lock();
    conn.execute_batch(
        "INSERT INTO active_context VALUES ('run_1', 'node_a1', 'active', 0, 'jev')",
    )
    .unwrap();
    conn.execute_batch("DELETE FROM active_context WHERE node_id = 'node_a1'").unwrap();
    assert_eq!(count(&conn, "SELECT count(*) FROM active_context"), 0);
    assert_eq!(count(&conn, "SELECT count(*) FROM dag_nodes WHERE id = 'node_a1'"), 1);
}

#[test]
fn active_context_rows_are_constrained() {
    let store = seeded();
    let conn = store.lock();
    rejects(
        &conn,
        "INSERT INTO active_context VALUES ('run_1', 'node_missing', 'active', 0, 'jev')",
    );
    rejects(&conn, "INSERT INTO active_context VALUES ('run_1', 'node_a1', 'inactive', 0, 'jev')");
    rejects(
        &conn,
        "INSERT INTO active_context VALUES ('run_1', 'node_a1', 'active', 0, 'planner')",
    );
    conn.execute_batch(
        "INSERT INTO active_context VALUES ('run_1', 'node_a1', 'active', 0, 'jev')",
    )
    .unwrap();
    rejects(&conn, "INSERT INTO active_context VALUES ('run_1', 'node_a2', 'active', 0, 'jev')");
}

#[test]
fn at_most_one_run_per_project_is_running() {
    let store = seeded();
    let conn = store.lock();
    rejects(
        &conn,
        "INSERT INTO runs (id, project_id, provider_id, model_id, system_prompt, status, started_at)
         VALUES ('run_2', 'proj_a', 'fake', 'fake-echo', '', 'running', 't4')",
    );
    conn.execute_batch(
        "INSERT INTO runs (id, project_id, provider_id, model_id, system_prompt, status, started_at)
         VALUES ('run_2', 'proj_b', 'fake', 'fake-echo', '', 'running', 't4')",
    )
    .unwrap();
}

#[test]
fn run_status_agrees_with_completion_fields_and_identity_is_required() {
    let store = seeded();
    let conn = store.lock();
    rejects(&conn, "UPDATE runs SET status = 'completed' WHERE id = 'run_1'");
    rejects(&conn, "UPDATE runs SET status = 'failed', completed_at = 't9' WHERE id = 'run_1'");
    rejects(&conn, "UPDATE runs SET provider_id = '' WHERE id = 'run_1'");
    rejects(&conn, "UPDATE runs SET model_id = '' WHERE id = 'run_1'");
    conn.execute_batch(
        "UPDATE runs SET status = 'failed', completed_at = 't9', error_code = 'provider_timeout'
         WHERE id = 'run_1'",
    )
    .unwrap();
}

#[test]
fn event_sequences_are_unique_positive_and_history_is_append_only() {
    let store = seeded();
    let conn = store.lock();
    conn.execute_batch("INSERT INTO events VALUES ('evt_1', 'run_1', 1, 'run.started', '{}', 't')")
        .unwrap();
    rejects(&conn, "INSERT INTO events VALUES ('evt_2', 'run_1', 1, 'run.started', '{}', 't')");
    rejects(&conn, "INSERT INTO events VALUES ('evt_2', 'run_1', 0, 'run.started', '{}', 't')");
    rejects(
        &conn,
        "INSERT INTO events VALUES ('evt_2', 'run_missing', 1, 'run.started', '{}', 't')",
    );
    let error = rejects(&conn, "UPDATE events SET type = 'x' WHERE id = 'evt_1'");
    assert!(error.contains("append-only"), "{error}");
    let error = rejects(&conn, "DELETE FROM events WHERE id = 'evt_1'");
    assert!(error.contains("append-only"), "{error}");
}
