//! Durable DAG records: typed nodes and edges.

use serde::{Deserialize, Serialize};

use super::ids::{EdgeId, NodeId};
use super::time::Timestamp;

/// A node's JSON object payload. Keys serialize in sorted order, so equal payloads always produce
/// identical JSON.
pub type Payload = serde_json::Map<String, serde_json::Value>;

/// A string that names no variant of a closed DAGOS enum.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {kind} `{value}`")]
pub struct UnknownVariant {
    pub kind: &'static str,
    pub value: String,
}

/// Defines a closed enum whose variants have fixed wire names, with `as_str`, `FromStr`, `Display`,
/// and serde support. Unknown names are rejected everywhere.
macro_rules! closed_enum {
    (
        $(#[$doc:meta])* $name:ident, $kind:literal {
            $($(#[$variant_doc:meta])* $variant:ident => $text:literal),+ $(,)?
        }
    ) => {
        $(#[$doc])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
            ::serde::Serialize, ::serde::Deserialize,
        )]
        pub enum $name {
            $($(#[$variant_doc])* #[serde(rename = $text)] $variant),+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),+
                }
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::domain::UnknownVariant;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($text => Ok($name::$variant),)+
                    _ => Err($crate::domain::UnknownVariant {
                        kind: $kind,
                        value: value.to_owned(),
                    }),
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

pub(crate) use closed_enum;

closed_enum!(
    /// The v1 node types.
    NodeType, "node type" {
        Task => "task",
        Artifact => "artifact",
        Observation => "observation",
        Decision => "decision",
        Result => "result",
        Conversation => "conversation",
    }
);

closed_enum!(
    /// The v1 edge types. An edge `from → to` reads "from <type> to", e.g. `a depends_on b`.
    EdgeType, "edge type" {
        DependsOn => "depends_on",
        Produces => "produces",
        ObservedFrom => "observed_from",
        RelatedTo => "related_to",
        Supersedes => "supersedes",
    }
);

/// A durable DAG node. Serializes to the `kiss://schemas/dag/v1` node contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DagNode {
    pub id: NodeId,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub payload: Payload,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// A durable, directed DAG edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DagEdge {
    pub id: EdgeId,
    pub from_node_id: NodeId,
    pub to_node_id: NodeId,
    #[serde(rename = "type")]
    pub edge_type: EdgeType,
    pub created_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn enums_round_trip_through_text_and_json() {
        for node_type in NodeType::ALL {
            assert_eq!(node_type.as_str().parse::<NodeType>().unwrap(), *node_type);
            let json = serde_json::to_value(node_type).unwrap();
            assert_eq!(json, json!(node_type.as_str()));
        }
        for edge_type in EdgeType::ALL {
            assert_eq!(edge_type.as_str().parse::<EdgeType>().unwrap(), *edge_type);
        }
        assert_eq!(NodeType::ALL.len(), 6);
        assert_eq!(EdgeType::ALL.len(), 5);
    }

    #[test]
    fn unknown_enum_values_are_rejected() {
        assert!("plan".parse::<NodeType>().is_err());
        assert!("Task".parse::<NodeType>().is_err());
        assert!("blocks".parse::<EdgeType>().is_err());
        assert!(serde_json::from_value::<NodeType>(json!("tool_call")).is_err());
        assert!(serde_json::from_value::<EdgeType>(json!("dependsOn")).is_err());
    }

    fn node() -> DagNode {
        let mut payload = Payload::new();
        payload.insert("zeta".into(), json!(1));
        payload.insert("alpha".into(), json!({"b": 2, "a": 1}));
        DagNode {
            id: NodeId::parse("node_000001").unwrap(),
            node_type: NodeType::Decision,
            payload,
            created_at: Timestamp::parse("2026-09-22T19:19:34.123Z").unwrap(),
            updated_at: Timestamp::parse("2026-09-22T19:19:34.123Z").unwrap(),
        }
    }

    #[test]
    fn node_serialization_is_deterministic() {
        let expected = r#"{"id":"node_000001","type":"decision","payload":{"alpha":{"a":1,"b":2},"zeta":1},"created_at":"2026-09-22T19:19:34.123Z","updated_at":"2026-09-22T19:19:34.123Z"}"#;
        assert_eq!(serde_json::to_string(&node()).unwrap(), expected);
        let reparsed: DagNode = serde_json::from_str(expected).unwrap();
        assert_eq!(reparsed, node());
        assert_eq!(serde_json::to_string(&reparsed).unwrap(), expected);
    }

    #[test]
    fn nodes_reject_unknown_fields_and_invalid_values() {
        let mut value = serde_json::to_value(node()).unwrap();
        value["project_id"] = json!("proj_1");
        assert!(serde_json::from_value::<DagNode>(value).is_err());

        let mut value = serde_json::to_value(node()).unwrap();
        value["payload"] = json!([1, 2]);
        assert!(serde_json::from_value::<DagNode>(value).is_err());

        let mut value = serde_json::to_value(node()).unwrap();
        value["id"] = json!("run_000001");
        assert!(serde_json::from_value::<DagNode>(value).is_err());
    }
}
