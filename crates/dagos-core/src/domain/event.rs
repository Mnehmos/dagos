//! Events: the ordered, append-only history of a run.
//!
//! Every state transition in a run is recorded as an event with a per-run sequence number that
//! starts at 1 and increases by 1. An event's `type` names the transition; its `payload` is a JSON
//! object whose shape is fixed per type.

use serde::{Deserialize, Serialize};

use super::dag::{EdgeType, NodeType};
use super::ids::{EdgeId, EventId, NodeId, RunId};
use super::ir::InferenceIr;
use super::jev::{ContextClassification, JevRequest};
use super::response::{EmissionRef, InferenceResponse};
use super::run::{ErrorCode, ModelId, ProviderId};
use super::time::Timestamp;

/// A recorded run event. Serializes flat: `{id, run_id, sequence, type, payload, created_at}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    pub id: EventId,
    pub run_id: RunId,
    pub sequence: u32,
    #[serde(flatten)]
    pub data: EventData,
    pub created_at: Timestamp,
}

/// The typed `type` + `payload` of an event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum EventData {
    /// A run began with this provider, model, and system prompt.
    #[serde(rename = "run.started")]
    RunStarted { provider_id: ProviderId, model_id: ModelId, system_prompt: String },
    /// The user's message was recorded as a durable `conversation` node: the run's task.
    #[serde(rename = "message.recorded")]
    MessageRecorded { node_id: NodeId, text: String },
    /// The run's active context was seeded with the previous run's members (`from_run_id` is
    /// `null` for a project's first run).
    #[serde(rename = "context.carried")]
    ContextCarried { from_run_id: Option<RunId>, node_ids: Vec<NodeId> },
    /// Jev was asked to classify context membership.
    #[serde(rename = "jev.requested")]
    JevRequested { jev_id: String, request: JevRequest },
    /// Jev's output passed validation and was applied to the active context.
    #[serde(rename = "jev.classified")]
    JevClassified { classification: ContextClassification },
    /// Jev's output violated the contract or the request and was discarded unapplied.
    #[serde(rename = "jev.rejected")]
    JevRejected { reason: String, output: String },
    /// The requested Jev failed or was rejected, so the fallback classifier `jev_id` classifies
    /// the same request instead. `reason` says why.
    #[serde(rename = "jev.fallback")]
    JevFallback { jev_id: String, reason: String },
    /// A node joined the run's active context.
    #[serde(rename = "context.added")]
    ContextAdded { node_id: NodeId },
    /// A node left the run's active context. The durable node is unaffected.
    #[serde(rename = "context.removed")]
    ContextRemoved { node_id: NodeId },
    /// The run's inference IR was compiled; this is exactly what the provider receives.
    #[serde(rename = "ir.compiled")]
    IrCompiled { ir: InferenceIr },
    /// The provider was called.
    #[serde(rename = "inference.started")]
    InferenceStarted { provider_id: ProviderId, model_id: ModelId },
    /// A fragment of streamed presentation prose. Never canonical state.
    #[serde(rename = "inference.delta")]
    InferenceDelta { text: String },
    /// The provider's raw final output, recorded verbatim before validation.
    #[serde(rename = "inference.completed")]
    InferenceCompleted { output: String },
    /// The final output was validated and its emissions applied, atomically with this event.
    #[serde(rename = "response.validated")]
    ResponseValidated { response: InferenceResponse },
    /// The final output was rejected; nothing from it entered the DAG.
    #[serde(rename = "response.rejected")]
    ResponseRejected { reason: String },
    /// An emission became a durable node.
    #[serde(rename = "dag.node_created")]
    DagNodeCreated {
        node_id: NodeId,
        node_type: NodeType,
        #[serde(rename = "ref")]
        emission_ref: EmissionRef,
    },
    /// An emission became a durable edge.
    #[serde(rename = "dag.edge_created")]
    DagEdgeCreated { edge_id: EdgeId, from: NodeId, to: NodeId, edge_type: EdgeType },
    /// The run finished and its results are durable.
    #[serde(rename = "run.completed")]
    RunCompleted {},
    /// The run stopped at an explicit failure. State committed before the failure remains intact.
    #[serde(rename = "run.failed")]
    RunFailed { error_code: ErrorCode, message: String },
}

impl EventData {
    /// Splits into the stored `(type, payload)` pair.
    pub fn to_parts(&self) -> (String, serde_json::Value) {
        let serde_json::Value::Object(mut object) =
            serde_json::to_value(self).expect("event data serializes to JSON")
        else {
            unreachable!("adjacently tagged enums serialize to objects")
        };
        let event_type = match object.remove("type") {
            Some(serde_json::Value::String(event_type)) => event_type,
            _ => unreachable!("adjacently tagged enums carry a string tag"),
        };
        let payload = object
            .remove("payload")
            .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
        (event_type, payload)
    }

    /// The event's `type`, e.g. `run.started`.
    pub fn event_type(&self) -> String {
        self.to_parts().0
    }

    /// Rebuilds event data from a stored `(type, payload)` pair, rejecting unknown types and
    /// payloads that do not match the type's shape.
    pub fn from_parts(
        event_type: &str,
        payload: serde_json::Value,
    ) -> Result<Self, serde_json::Error> {
        serde_json::from_value(serde_json::json!({ "type": event_type, "payload": payload }))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn started() -> EventData {
        EventData::RunStarted {
            provider_id: ProviderId::parse("fake").unwrap(),
            model_id: ModelId::parse("fake-echo").unwrap(),
            system_prompt: "Be brief.".into(),
        }
    }

    #[test]
    fn event_data_splits_into_type_and_object_payload() {
        let (event_type, payload) = started().to_parts();
        assert_eq!(event_type, "run.started");
        assert_eq!(
            payload,
            json!({"provider_id": "fake", "model_id": "fake-echo", "system_prompt": "Be brief."})
        );
        let (event_type, payload) = EventData::RunCompleted {}.to_parts();
        assert_eq!((event_type.as_str(), payload), ("run.completed", json!({})));
    }

    #[test]
    fn event_data_round_trips_through_parts() {
        for data in [
            started(),
            EventData::RunCompleted {},
            EventData::RunFailed {
                error_code: ErrorCode::ProviderTimeout,
                message: "no response within 30s".into(),
            },
        ] {
            let (event_type, payload) = data.to_parts();
            assert_eq!(EventData::from_parts(&event_type, payload).unwrap(), data);
        }
    }

    #[test]
    fn unknown_types_and_mismatched_payloads_are_rejected() {
        assert!(EventData::from_parts("run.exploded", json!({})).is_err());
        assert!(EventData::from_parts("run.failed", json!({"message": "x"})).is_err());
        assert!(
            EventData::from_parts("run.failed", json!({"error_code": "nope", "message": "x"}))
                .is_err()
        );
    }

    #[test]
    fn events_serialize_flat_and_deterministically() {
        let event = Event {
            id: EventId::parse("evt_000001").unwrap(),
            run_id: RunId::parse("run_000001").unwrap(),
            sequence: 1,
            data: EventData::RunCompleted {},
            created_at: Timestamp::parse("2026-09-22T19:19:34.123Z").unwrap(),
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"id":"evt_000001","run_id":"run_000001","sequence":1,"type":"run.completed","payload":{},"created_at":"2026-09-22T19:19:34.123Z"}"#
        );
    }
}
