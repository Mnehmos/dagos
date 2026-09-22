//! Versioned schema migrations, tracked with SQLite's `user_version` header field.
//!
//! Migration `i` (0-based) upgrades the schema from version `i` to `i + 1`. Each migration runs in
//! its own transaction together with the version bump, so a failed migration leaves the database
//! at the previous version with no partial schema. Released migrations are never edited; schema
//! changes append a new migration.

use rusqlite::Connection;

use super::StoreError;

/// Every migration shipped with this build, in order.
pub(crate) const MIGRATIONS: &[&str] = &[
    include_str!("migrations/0001_initial.sql"),
    include_str!("migrations/0002_run_defaults.sql"),
];

/// The schema version this build creates and understands.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

pub(crate) fn schema_version(conn: &Connection) -> rusqlite::Result<u32> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
}

/// Applies every migration newer than the database's current version.
pub(crate) fn migrate(conn: &mut Connection, migrations: &[&str]) -> Result<(), StoreError> {
    let current = schema_version(conn)?;
    let supported = migrations.len() as u32;
    if current > supported {
        return Err(StoreError::SchemaTooNew { found: current, supported });
    }
    for (index, sql) in migrations.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index as u32 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_MIGRATION_2: &str =
        "ALTER TABLE projects ADD COLUMN description TEXT NOT NULL DEFAULT '';";

    fn fresh() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        conn
    }

    fn schema(conn: &Connection) -> Vec<(String, String, Option<String>)> {
        let mut statement =
            conn.prepare("SELECT type, name, sql FROM sqlite_master ORDER BY type, name").unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn fresh_database_migrates_to_latest_version() {
        let mut conn = fresh();
        assert_eq!(schema_version(&conn).unwrap(), 0);
        migrate(&mut conn, MIGRATIONS).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
        let names: Vec<String> = schema(&conn)
            .into_iter()
            .filter(|(kind, _, _)| kind == "table")
            .map(|(_, name, _)| name)
            .collect();
        assert_eq!(
            names,
            [
                "active_context",
                "dag_edges",
                "dag_nodes",
                "events",
                "projects",
                "run_defaults",
                "runs"
            ]
        );
    }

    #[test]
    fn a_version_1_database_upgrades_to_run_defaults_with_its_data_intact() {
        let mut conn = fresh();
        migrate(&mut conn, &MIGRATIONS[..1]).unwrap();
        conn.execute_batch(
            "INSERT INTO projects (id, name, created_at) VALUES ('proj_1', 'kept', 't0');
             INSERT INTO dag_nodes VALUES ('node_1', 'proj_1', 'task', '{}', 't1', 't1');",
        )
        .unwrap();

        migrate(&mut conn, MIGRATIONS).unwrap();

        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
        let nodes: i64 =
            conn.query_row("SELECT count(*) FROM dag_nodes", [], |row| row.get(0)).unwrap();
        assert_eq!(nodes, 1);
        conn.execute(
            "INSERT INTO run_defaults VALUES ('proj_1', 'fake', 'fake-echo', 'Be brief.', 't2')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn fresh_databases_initialize_identically() {
        let (mut a, mut b) = (fresh(), fresh());
        migrate(&mut a, MIGRATIONS).unwrap();
        migrate(&mut b, MIGRATIONS).unwrap();
        assert_eq!(schema(&a), schema(&b));
    }

    #[test]
    fn migrating_an_up_to_date_database_is_a_no_op() {
        let mut conn = fresh();
        migrate(&mut conn, MIGRATIONS).unwrap();
        let before = schema(&conn);
        migrate(&mut conn, MIGRATIONS).unwrap();
        assert_eq!(schema(&conn), before);
        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn existing_database_upgrades_through_new_migrations_and_keeps_data() {
        let mut conn = fresh();
        migrate(&mut conn, &MIGRATIONS[..1]).unwrap();
        conn.execute(
            "INSERT INTO projects (id, name, created_at) VALUES ('proj_1', 'kept', 't0')",
            [],
        )
        .unwrap();

        let upgraded = [MIGRATIONS[0], TEST_MIGRATION_2];
        migrate(&mut conn, &upgraded).unwrap();

        assert_eq!(schema_version(&conn).unwrap(), 2);
        let (name, description): (String, String) = conn
            .query_row("SELECT name, description FROM projects WHERE id = 'proj_1'", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!((name.as_str(), description.as_str()), ("kept", ""));
    }

    #[test]
    fn failed_migration_rolls_back_to_previous_version() {
        let mut conn = fresh();
        migrate(&mut conn, MIGRATIONS).unwrap();
        let before = schema(&conn);

        let mut broken = MIGRATIONS.to_vec();
        broken.push("CREATE TABLE half_done (a TEXT); SELECT no_such_fn();");
        assert!(migrate(&mut conn, &broken).is_err());

        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
        assert_eq!(schema(&conn), before, "no partial schema may survive");
    }

    #[test]
    fn database_newer_than_this_build_is_rejected() {
        let mut conn = fresh();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
        let error = migrate(&mut conn, MIGRATIONS).unwrap_err();
        assert!(matches!(
            error,
            StoreError::SchemaTooNew { found, supported }
                if found == SCHEMA_VERSION + 1 && supported == SCHEMA_VERSION
        ));
    }
}
