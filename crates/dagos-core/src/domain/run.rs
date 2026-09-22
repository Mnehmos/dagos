//! Runs: one pass of the pipeline for one user message, with explicit provider and model identity.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::dag::closed_enum;
use super::ids::{ProjectId, RunId};
use super::time::Timestamp;

/// A value that is not a valid provider or model identifier.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind} `{value}`: {rule}")]
pub struct IdentityError {
    kind: &'static str,
    value: String,
    rule: &'static str,
}

/// Identifies an inference provider adapter, e.g. `fake` or `openai-compatible`.
///
/// 1–64 characters: lowercase letters, digits, `.`, `_`, `-`, starting with a letter or digit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProviderId(String);

impl ProviderId {
    pub fn parse(value: impl Into<String>) -> Result<Self, IdentityError> {
        let value = value.into();
        let valid = (1..=64).contains(&value.len())
            && value.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && value.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(IdentityError {
                kind: "provider id",
                value,
                rule: "expected 1-64 of [a-z0-9._-], starting with a letter or digit",
            })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A model identifier, passed to the provider adapter unchanged (e.g. `anthropic/claude-sonnet-4.5`).
/// The core never interprets it.
///
/// 1–200 printable ASCII characters without whitespace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ModelId(String);

impl ModelId {
    pub fn parse(value: impl Into<String>) -> Result<Self, IdentityError> {
        let value = value.into();
        let valid = (1..=200).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_graphic());
        if valid {
            Ok(Self(value))
        } else {
            Err(IdentityError {
                kind: "model id",
                value,
                rule: "expected 1-200 printable ASCII characters without whitespace",
            })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

macro_rules! string_conversions {
    ($name:ident, $error:ident) => {
        impl TryFrom<String> for $name {
            type Error = $error;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_conversions!(ProviderId, IdentityError);
string_conversions!(ModelId, IdentityError);

closed_enum!(
    /// Lifecycle state of a run. `running` is the only non-terminal state.
    RunStatus, "run status" {
        Running => "running",
        Completed => "completed",
        Failed => "failed",
    }
);

closed_enum!(
    /// Why a run failed. Stable, machine-readable codes recorded on the run and its `run.failed` event.
    ErrorCode, "error code" {
        /// The Jev adapter could not produce a classification.
        JevFailed => "jev_failed",
        /// Jev output violated the classification-only contract.
        JevInvalidOutput => "jev_invalid_output",
        /// The compiled IR failed validation.
        IrInvalid => "ir_invalid",
        /// The provider adapter reported an error.
        ProviderFailed => "provider_failed",
        /// The provider did not finish before the deadline.
        ProviderTimeout => "provider_timeout",
        /// The provider's final output was not valid `kiss.inference-response.v1`.
        ResponseInvalid => "response_invalid",
        /// A valid response carried emissions that cannot be applied to the DAG.
        EmissionRejected => "emission_rejected",
        /// The run was still running when DAGOS stopped.
        Interrupted => "interrupted",
        /// Persistence or another internal step failed.
        Internal => "internal",
    }
);

/// Provider, model, and system prompt for a run.
///
/// Changing this configuration changes who performs inference and with which instructions; it
/// never changes DAG semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunConfig {
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub system_prompt: String,
}

/// One pass of the pipeline. Provider and model identity are recorded on every run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub id: RunId,
    pub project_id: ProjectId,
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    pub system_prompt: String,
    pub status: RunStatus,
    pub started_at: Timestamp,
    pub completed_at: Option<Timestamp>,
    pub error_code: Option<ErrorCode>,
}

impl Run {
    /// The configuration this run executed with.
    pub fn config(&self) -> RunConfig {
        RunConfig {
            provider_id: self.provider_id.clone(),
            model_id: self.model_id.clone(),
            system_prompt: self.system_prompt.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn provider_ids_are_validated() {
        for valid in ["fake", "openai-compatible", "z.ai", "open_router", "a", "4o"] {
            assert!(ProviderId::parse(valid).is_ok(), "{valid}");
        }
        for invalid in ["", "Fake", "-fake", "open router", "fake/1", &"a".repeat(65)] {
            assert!(ProviderId::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn model_ids_are_opaque_but_bounded() {
        for valid in [
            "fake-echo",
            "anthropic/claude-sonnet-4.5",
            "llama3.1:8b",
            "Qwen/Qwen2.5-Coder-32B-Instruct",
        ] {
            assert!(ModelId::parse(valid).is_ok(), "{valid}");
        }
        for invalid in ["", "gpt 4", "tab\tmodel", "émoji", &"m".repeat(201)] {
            assert!(ModelId::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn statuses_and_error_codes_are_closed() {
        assert_eq!("failed".parse::<RunStatus>().unwrap(), RunStatus::Failed);
        assert!("cancelled".parse::<RunStatus>().is_err());
        assert_eq!(
            serde_json::to_value(ErrorCode::ProviderTimeout).unwrap(),
            json!("provider_timeout")
        );
        assert!(serde_json::from_value::<ErrorCode>(json!("retry_later")).is_err());
    }

    #[test]
    fn run_serialization_is_deterministic() {
        let run = Run {
            id: RunId::parse("run_000001").unwrap(),
            project_id: ProjectId::parse("proj_000001").unwrap(),
            provider_id: ProviderId::parse("fake").unwrap(),
            model_id: ModelId::parse("fake-echo").unwrap(),
            system_prompt: "Be brief.".into(),
            status: RunStatus::Failed,
            started_at: Timestamp::parse("2026-09-22T19:19:34.123Z").unwrap(),
            completed_at: Some(Timestamp::parse("2026-09-22T19:19:35.000Z").unwrap()),
            error_code: Some(ErrorCode::ResponseInvalid),
        };
        let expected = r#"{"id":"run_000001","project_id":"proj_000001","provider_id":"fake","model_id":"fake-echo","system_prompt":"Be brief.","status":"failed","started_at":"2026-09-22T19:19:34.123Z","completed_at":"2026-09-22T19:19:35.000Z","error_code":"response_invalid"}"#;
        assert_eq!(serde_json::to_string(&run).unwrap(), expected);
        assert_eq!(serde_json::from_str::<Run>(expected).unwrap(), run);
    }
}
