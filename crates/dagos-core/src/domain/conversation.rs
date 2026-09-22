//! Conversation turns: the payload of the `conversation` nodes the runtime records for each run's
//! user message and, once a response is validated, the assistant's reply.

use serde::{Deserialize, Serialize};

use super::dag::{Payload, closed_enum};

closed_enum!(
    /// Who spoke a conversation turn.
    Role, "conversation role" {
        User => "user",
        Assistant => "assistant",
    }
);

/// A conversation turn, stored as the payload `{"role": ..., "text": ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub role: Role,
    pub text: String,
}

impl ConversationTurn {
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, text: text.into() }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self { role: Role::Assistant, text: text.into() }
    }

    pub fn to_payload(&self) -> Payload {
        match serde_json::to_value(self).expect("conversation turns serialize") {
            serde_json::Value::Object(payload) => payload,
            _ => unreachable!("structs serialize to objects"),
        }
    }

    /// Reads a turn from a conversation node's payload; `None` if the payload is not a turn.
    /// Extra payload keys are ignored.
    pub fn from_payload(payload: &Payload) -> Option<Self> {
        serde_json::from_value(serde_json::Value::Object(payload.clone())).ok()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn turns_round_trip_through_payloads() {
        let turn = ConversationTurn::user("Add restart tests");
        let payload = turn.to_payload();
        assert_eq!(
            serde_json::Value::Object(payload.clone()),
            json!({"role": "user", "text": "Add restart tests"})
        );
        assert_eq!(ConversationTurn::from_payload(&payload), Some(turn));
    }

    #[test]
    fn non_turn_payloads_are_not_turns() {
        for value in [json!({"text": "no role"}), json!({"role": "system", "text": "x"})] {
            let serde_json::Value::Object(payload) = value else { unreachable!() };
            assert_eq!(ConversationTurn::from_payload(&payload), None);
        }
    }
}
