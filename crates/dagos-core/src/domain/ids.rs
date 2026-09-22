//! Typed identifiers.
//!
//! Every record kind has its own ID type with a fixed prefix (`node_…`, `run_…`), so a run ID can
//! never be passed where a node ID is expected, and IDs are recognizable in events and the
//! inspector. An ID is `<prefix>_` followed by 1–64 lowercase ASCII letters or digits.

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// A string that is not a valid ID of the expected kind.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid {kind} id `{value}`: expected `{prefix}_` followed by 1-64 lowercase letters or digits"
)]
pub struct IdError {
    kind: &'static str,
    prefix: &'static str,
    value: String,
}

/// Produces the unique part of new IDs. Injected so tests can create deterministic IDs.
pub trait IdGenerator: Send + Sync {
    /// Returns a fresh suffix for an ID with `prefix`: 1–64 lowercase letters or digits.
    fn next_suffix(&self, prefix: &str) -> String;
}

/// Time-ordered random IDs (UUIDv7), the production generator.
#[derive(Debug, Default, Clone, Copy)]
pub struct RandomIds;

impl IdGenerator for RandomIds {
    fn next_suffix(&self, _prefix: &str) -> String {
        uuid::Uuid::now_v7().simple().to_string()
    }
}

/// Deterministic IDs counting up per prefix (`node_000001`, `node_000002`, `run_000001`, …).
#[derive(Debug, Default)]
pub struct SequentialIds {
    counters: Mutex<HashMap<String, u64>>,
}

impl IdGenerator for SequentialIds {
    fn next_suffix(&self, prefix: &str) -> String {
        let mut counters = self.counters.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let counter = counters.entry(prefix.to_owned()).or_insert(0);
        *counter += 1;
        format!("{counter:06}")
    }
}

fn is_valid_suffix(suffix: &str) -> bool {
    (1..=64).contains(&suffix.len())
        && suffix.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

macro_rules! define_id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal, $kind:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// The fixed prefix of this ID kind.
            pub const PREFIX: &'static str = $prefix;

            /// Validates `value` as an ID of this kind.
            pub fn parse(value: impl Into<String>) -> Result<Self, IdError> {
                let value = value.into();
                let valid = value
                    .strip_prefix($prefix)
                    .and_then(|rest| rest.strip_prefix('_'))
                    .is_some_and(is_valid_suffix);
                if valid {
                    Ok(Self(value))
                } else {
                    Err(IdError { kind: $kind, prefix: $prefix, value })
                }
            }

            /// Creates a new ID of this kind from `ids`.
            ///
            /// # Panics
            /// If the generator breaks its contract by returning an invalid suffix.
            pub fn generate(ids: &dyn IdGenerator) -> Self {
                let suffix = ids.next_suffix($prefix);
                Self::parse(format!("{}_{}", $prefix, suffix))
                    .expect("IdGenerator returned an invalid suffix")
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(value)
            }
        }

        impl From<$name> for String {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

define_id!(
    /// Identifies a project.
    ProjectId, "proj", "project"
);
define_id!(
    /// Identifies a durable DAG node.
    NodeId, "node", "node"
);
define_id!(
    /// Identifies a durable DAG edge.
    EdgeId, "edge", "edge"
);
define_id!(
    /// Identifies a run.
    RunId, "run", "run"
);
define_id!(
    /// Identifies an event.
    EventId, "evt", "event"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ids_with_their_own_prefix_only() {
        assert_eq!(NodeId::parse("node_01abc").unwrap().as_str(), "node_01abc");
        assert!(NodeId::parse("run_01abc").is_err());
        assert!(RunId::parse("run_01abc").is_ok());
        assert!(NodeId::parse("node_").is_err());
        assert!(NodeId::parse("node").is_err());
        assert!(NodeId::parse("node_ABC").is_err());
        assert!(NodeId::parse("node_a-b").is_err());
        assert!(NodeId::parse("nodes_abc").is_err());
        assert!(NodeId::parse(format!("node_{}", "a".repeat(64))).is_ok());
        assert!(NodeId::parse(format!("node_{}", "a".repeat(65))).is_err());
    }

    #[test]
    fn serde_round_trips_and_rejects_invalid_ids() {
        let id = EdgeId::parse("edge_42").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"edge_42\"");
        assert_eq!(serde_json::from_str::<EdgeId>(&json).unwrap(), id);
        assert!(serde_json::from_str::<EdgeId>("\"node_42\"").is_err());
        assert!(serde_json::from_str::<EdgeId>("42").is_err());
    }

    #[test]
    fn sequential_ids_are_deterministic_per_prefix() {
        let ids = SequentialIds::default();
        assert_eq!(NodeId::generate(&ids).as_str(), "node_000001");
        assert_eq!(NodeId::generate(&ids).as_str(), "node_000002");
        assert_eq!(RunId::generate(&ids).as_str(), "run_000001");
    }

    #[test]
    fn random_ids_are_valid_and_unique() {
        let a = NodeId::generate(&RandomIds);
        let b = NodeId::generate(&RandomIds);
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), "node_".len() + 32);
    }
}
