//! The structured inference response (`kiss.inference-response.v1`).
//!
//! `presentation.prose` is for people and never becomes canonical state. `emissions` are the only
//! structured state a response can add to the DAG, and only after DAGOS validates them.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::dag::{EdgeType, NodeType, Payload};
use super::ids::NodeId;
use super::schema::InferenceResponseSchema;

/// A provider's final output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceResponse {
    pub schema: InferenceResponseSchema,
    pub presentation: Presentation,
    pub emissions: Vec<Emission>,
    /// Requested tool invocations: recorded, never executed in v0.1.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Provider-specific details: recorded, never interpreted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Payload>,
}

/// Prose for people. Never canonical state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Presentation {
    pub prose: String,
}

/// One proposed addition to the durable DAG.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Emission {
    /// A new node, named by a ref local to this response.
    Node {
        #[serde(rename = "ref")]
        reference: EmissionRef,
        #[serde(rename = "type")]
        node_type: NodeType,
        payload: Payload,
    },
    /// A new edge `from <type> to`.
    Edge {
        from: Endpoint,
        to: Endpoint,
        #[serde(rename = "type")]
        edge_type: EdgeType,
    },
}

/// A tool invocation a model asked for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub name: String,
    pub arguments: Payload,
}

/// A string that is neither a valid emission ref nor a node ID.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid emission reference `{0}`: expected a local ref ([A-Za-z][A-Za-z0-9_-]{{0,63}}, not \
     starting with `node_`) or a node ID"
)]
pub struct EmissionRefError(String);

/// A response-local name for an emitted node, e.g. `obs1`. Never starts with `node_`, so it can
/// never be mistaken for a durable node ID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EmissionRef(String);

impl EmissionRef {
    pub fn parse(value: impl Into<String>) -> Result<Self, EmissionRefError> {
        let value = value.into();
        let mut bytes = value.bytes();
        let valid = value.len() <= 64
            && bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
            && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            && !value.starts_with("node_");
        if valid { Ok(Self(value)) } else { Err(EmissionRefError(value)) }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for EmissionRef {
    type Error = EmissionRefError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<EmissionRef> for String {
    fn from(reference: EmissionRef) -> Self {
        reference.0
    }
}

impl fmt::Display for EmissionRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An edge endpoint: a node emitted in the same response, or an existing node shown in the IR.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Endpoint {
    Ref(EmissionRef),
    Node(NodeId),
}

impl TryFrom<String> for Endpoint {
    type Error = EmissionRefError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.starts_with("node_") {
            NodeId::parse(value.clone()).map(Endpoint::Node).map_err(|_| EmissionRefError(value))
        } else {
            EmissionRef::parse(value).map(Endpoint::Ref)
        }
    }
}

impl From<Endpoint> for String {
    fn from(endpoint: Endpoint) -> Self {
        endpoint.to_string()
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Endpoint::Ref(reference) => f.write_str(reference.as_str()),
            Endpoint::Node(node_id) => f.write_str(node_id.as_str()),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn refs_cannot_look_like_node_ids() {
        for valid in ["obs1", "a", "Decision-2", "x_y"] {
            assert!(EmissionRef::parse(valid).is_ok(), "{valid}");
        }
        for invalid in ["", "1abc", "node_1", "has space", "dot.ted", &"a".repeat(65)] {
            assert!(EmissionRef::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn endpoints_are_refs_or_node_ids() {
        let parse = |text: &str| serde_json::from_value::<Endpoint>(json!(text));
        assert_eq!(parse("obs1").unwrap(), Endpoint::Ref(EmissionRef::parse("obs1").unwrap()));
        assert_eq!(
            parse("node_000001").unwrap(),
            Endpoint::Node(NodeId::parse("node_000001").unwrap())
        );
        assert!(parse("node_BAD").is_err());
        assert!(parse("run_000001x!").is_err());
    }

    #[test]
    fn emissions_serialize_with_a_kind_tag() {
        let node = Emission::Node {
            reference: EmissionRef::parse("obs1").unwrap(),
            node_type: NodeType::Observation,
            payload: Payload::new(),
        };
        assert_eq!(
            serde_json::to_value(&node).unwrap(),
            json!({"kind": "node", "ref": "obs1", "type": "observation", "payload": {}})
        );
        let edge: Emission = serde_json::from_value(
            json!({"kind": "edge", "from": "obs1", "to": "node_000001", "type": "observed_from"}),
        )
        .unwrap();
        assert!(matches!(edge, Emission::Edge { edge_type: EdgeType::ObservedFrom, .. }));
        assert!(serde_json::from_value::<Emission>(json!({"kind": "plan", "steps": []})).is_err());
        assert!(
            serde_json::from_value::<Emission>(
                json!({"kind": "node", "ref": "a", "type": "task", "payload": {}, "extra": 1})
            )
            .is_err()
        );
    }
}
