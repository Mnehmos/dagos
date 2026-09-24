//! Event repository: each run's append-only, ordered history.
//!
//! Sequence numbers are assigned here, starting at 1 and increasing by exactly 1 per run. Events can
//! only be appended while the run is running, and stored events are never updated or deleted.

use rusqlite::{Row, params};

use super::sql::Json;
use super::{StoreError, Tx};
use crate::domain::{Event, EventData, EventId, RunId};

const EVENT_COLUMNS: &str = "id, run_id, sequence, type, payload_json, created_at";

fn event_from_row(row: &Row<'_>) -> rusqlite::Result<Event> {
    let event_type: String = row.get(3)?;
    let payload = row.get::<_, Json<serde_json::Value>>(4)?.0;
    let data = EventData::from_parts(&event_type, payload).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(Event {
        id: row.get(0)?,
        run_id: row.get(1)?,
        sequence: row.get(2)?,
        data,
        created_at: row.get(5)?,
    })
}

impl Tx<'_> {
    /// Appends `data` to a running run's history with the next sequence number.
    pub fn append_event(&self, run_id: &RunId, data: EventData) -> Result<Event, StoreError> {
        self.require_running(run_id)?;
        let sequence: u32 = self.conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM events WHERE run_id = ?1",
            [run_id],
            |row| row.get(0),
        )?;
        let event = Event {
            id: EventId::generate(self.ids),
            run_id: run_id.clone(),
            sequence,
            data,
            created_at: self.now(),
        };
        let (event_type, payload) = event.data.to_parts();
        self.conn.execute(
            &format!("INSERT INTO events ({EVENT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6)"),
            params![
                event.id,
                event.run_id,
                event.sequence,
                event_type,
                Json(&payload),
                event.created_at
            ],
        )?;
        if let Some(appended) = &self.appended {
            appended.borrow_mut().push(event.clone());
        }
        Ok(event)
    }

    /// The run's full history in sequence order.
    pub fn events(&self, run_id: &RunId) -> Result<Vec<Event>, StoreError> {
        self.events_after(run_id, 0)
    }

    /// The run's events of the given `types` (e.g. `tool.completed`), in sequence order. Cheaper
    /// than [`Tx::events`] when a run's many `inference.delta` events are not needed.
    pub fn events_of_types(
        &self,
        run_id: &RunId,
        types: &[&str],
    ) -> Result<Vec<Event>, StoreError> {
        let placeholders = vec!["?"; types.len()].join(", ");
        let mut statement = self.conn.prepare(&format!(
            "SELECT {EVENT_COLUMNS} FROM events \
             WHERE run_id = ? AND type IN ({placeholders}) ORDER BY sequence"
        ))?;
        let mut parameters: Vec<&dyn rusqlite::ToSql> = vec![run_id];
        parameters.extend(types.iter().map(|kind| kind as &dyn rusqlite::ToSql));
        let events = statement
            .query_map(parameters.as_slice(), event_from_row)?
            .collect::<Result<_, _>>()?;
        Ok(events)
    }

    /// The run's events with a sequence number greater than `after`, in sequence order.
    pub fn events_after(&self, run_id: &RunId, after: u32) -> Result<Vec<Event>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {EVENT_COLUMNS} FROM events WHERE run_id = ?1 AND sequence > ?2 ORDER BY sequence"
        ))?;
        let events = statement
            .query_map(params![run_id, after], event_from_row)?
            .collect::<Result<_, _>>()?;
        Ok(events)
    }
}
