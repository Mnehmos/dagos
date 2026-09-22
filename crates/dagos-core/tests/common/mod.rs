//! Shared helpers for integration tests.
#![allow(dead_code)]

use dagos_core::domain::{Payload, SequentialIds, SteppingClock, Timestamp};
use dagos_core::store::Store;
use serde_json::Value;

/// The first instant a deterministic store's clock reports.
pub const EPOCH: &str = "2026-01-01T00:00:00.000Z";

/// An in-memory store whose clock advances 1s per reading and whose IDs count up per prefix.
pub fn memory_store() -> Store {
    deterministic(Store::open_in_memory().unwrap())
}

/// Makes `store` deterministic: stepping clock starting at [`EPOCH`], sequential IDs.
pub fn deterministic(store: Store) -> Store {
    store
        .with_clock(SteppingClock::new(Timestamp::parse(EPOCH).unwrap(), 1000))
        .with_ids(SequentialIds::default())
}

/// Converts a `json!({...})` literal into a node payload.
pub fn payload(value: Value) -> Payload {
    match value {
        Value::Object(map) => map,
        other => panic!("payload must be a JSON object, got {other}"),
    }
}
