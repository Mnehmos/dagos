//! The deterministic, offline fake Jev.

use std::collections::BTreeSet;

use async_trait::async_trait;

use super::jev::{JevClassifier, JevError};
use crate::domain::{
    Classification, ContextClassification, EdgeType, JevContextSchema, JevRequest,
    NodeClassification, NodeId, NodeType,
};

/// Conversation turns the fake keeps active by default.
pub const DEFAULT_CONVERSATION_WINDOW: usize = 6;

/// A deterministic Jev that needs no network or model.
///
/// Its default policy classifies every candidate:
/// 1. a candidate superseded by another (the target of a `supersedes` edge) is `inactive`;
/// 2. a `conversation` candidate older than the most recent `conversation_window` conversation
///    candidates is `inactive`;
/// 3. every other candidate is `active`.
///
/// The same request always yields byte-identical output, so recorded classifications can be
/// replayed. For failure testing it can instead return scripted raw output, be unavailable, or hang.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeJev {
    mode: Mode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    Policy { conversation_window: usize },
    Scripted(String),
    Unavailable(String),
    Hanging,
}

impl FakeJev {
    /// The default policy with [`DEFAULT_CONVERSATION_WINDOW`].
    pub fn new() -> Self {
        Self::with_conversation_window(DEFAULT_CONVERSATION_WINDOW)
    }

    /// The default policy, keeping the most recent `conversation_window` conversation turns.
    pub fn with_conversation_window(conversation_window: usize) -> Self {
        Self { mode: Mode::Policy { conversation_window } }
    }

    /// Returns `raw` verbatim for every request, valid or not.
    pub fn scripted(raw: impl Into<String>) -> Self {
        Self { mode: Mode::Scripted(raw.into()) }
    }

    /// Fails every request, as an unreachable endpoint would.
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self { mode: Mode::Unavailable(reason.into()) }
    }

    /// Never answers, as a hung endpoint would.
    pub fn hanging() -> Self {
        Self { mode: Mode::Hanging }
    }

    /// The default policy's classification of `request`.
    pub fn classify_by_policy(
        request: &JevRequest,
        conversation_window: usize,
    ) -> ContextClassification {
        let superseded: BTreeSet<&NodeId> = request
            .edges
            .iter()
            .filter(|edge| edge.edge_type == EdgeType::Supersedes)
            .map(|edge| &edge.to)
            .collect();
        let recent_conversation: BTreeSet<&NodeId> = request
            .candidates
            .iter()
            .rev()
            .filter(|candidate| candidate.node_type == NodeType::Conversation)
            .take(conversation_window)
            .map(|candidate| &candidate.node_id)
            .collect();
        let classifications = request
            .candidates
            .iter()
            .map(|candidate| {
                let active = !superseded.contains(&candidate.node_id)
                    && (candidate.node_type != NodeType::Conversation
                        || recent_conversation.contains(&candidate.node_id));
                NodeClassification {
                    node_id: candidate.node_id.clone(),
                    classification: if active {
                        Classification::Active
                    } else {
                        Classification::Inactive
                    },
                }
            })
            .collect();
        ContextClassification { schema: JevContextSchema, classifications, tools: Vec::new() }
    }
}

impl Default for FakeJev {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl JevClassifier for FakeJev {
    fn id(&self) -> &str {
        "fake-jev"
    }

    async fn classify(&self, request: &JevRequest) -> Result<String, JevError> {
        match &self.mode {
            Mode::Policy { conversation_window } => {
                let output = Self::classify_by_policy(request, *conversation_window);
                Ok(serde_json::to_string(&output).expect("classifications serialize"))
            }
            Mode::Scripted(raw) => Ok(raw.clone()),
            Mode::Unavailable(reason) => Err(JevError(reason.clone())),
            Mode::Hanging => std::future::pending().await,
        }
    }
}
