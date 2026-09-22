//! Versioned JSON Schema contracts and their validation.
//!
//! The schemas in `specs/001-core-runtime/contracts` are the language-neutral source of truth for
//! every machine interface in DAGOS; they are embedded here verbatim. Documents crossing a boundary
//! (Jev output, compiled IR, provider responses) are validated against them and rejected when
//! invalid. Rust types in [`crate::domain`] mirror the schemas, and tests keep the two in agreement.

use std::fmt;
use std::sync::LazyLock;

use serde::de::DeserializeOwned;
use serde_json::Value;

/// A versioned machine contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Contract {
    /// `kiss://schemas/dag/v1`: one durable DAG node.
    DagNode,
    /// `kiss://schemas/jev-request/v1`: what Jev may consider when classifying.
    JevRequest,
    /// `kiss://schemas/jev-context/v1`: Jev classification output.
    JevContext,
    /// `kiss://schemas/inference-ir/v1`: the provider-facing inference IR.
    InferenceIr,
    /// `kiss://schemas/inference-response/v1`: structured provider output.
    InferenceResponse,
}

impl Contract {
    pub const ALL: [Contract; 5] = [
        Contract::DagNode,
        Contract::JevRequest,
        Contract::JevContext,
        Contract::InferenceIr,
        Contract::InferenceResponse,
    ];

    /// The schema document, exactly as published in `specs/001-core-runtime/contracts`.
    pub fn schema_source(self) -> &'static str {
        match self {
            Contract::DagNode => {
                include_str!("../../../specs/001-core-runtime/contracts/dag.schema.json")
            }
            Contract::JevRequest => {
                include_str!("../../../specs/001-core-runtime/contracts/jev-request.schema.json")
            }
            Contract::JevContext => {
                include_str!("../../../specs/001-core-runtime/contracts/jev-context.schema.json")
            }
            Contract::InferenceIr => {
                include_str!("../../../specs/001-core-runtime/contracts/inference-ir.schema.json")
            }
            Contract::InferenceResponse => include_str!(
                "../../../specs/001-core-runtime/contracts/inference-response.schema.json"
            ),
        }
    }

    /// The schema's `$id`, e.g. `kiss://schemas/dag/v1`.
    pub fn schema_id(self) -> &'static str {
        compiled(self).id
    }

    /// Validates `document` against this contract, reporting every violation.
    pub fn validate(self, document: &Value) -> Result<(), ContractViolation> {
        let errors: Vec<String> = compiled(self)
            .validator
            .iter_errors(document)
            .map(|error| {
                let path = error.instance_path().to_string();
                let at = if path.is_empty() { "/" } else { path.as_str() };
                format!("at {at}: {error}")
            })
            .collect();
        if errors.is_empty() { Ok(()) } else { Err(ContractViolation { contract: self, errors }) }
    }

    /// Parses untrusted text as a document of this contract, failing closed: the text must be
    /// JSON, satisfy the schema, and deserialize into `T` (the typed mirror of the schema).
    pub fn parse<T: DeserializeOwned>(self, raw: &str) -> Result<T, ContractError> {
        let document: Value = serde_json::from_str(raw).map_err(|error| {
            ContractError::NotJson { contract: self, reason: error.to_string() }
        })?;
        self.validate(&document)?;
        serde_json::from_value(document).map_err(|error| {
            // Unreachable while the Rust mirror matches the schema; still rejected, never trusted.
            ContractError::Violation(ContractViolation {
                contract: self,
                errors: vec![format!("at /: {error}")],
            })
        })
    }
}

impl fmt::Display for Contract {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.schema_id())
    }
}

/// A document that does not satisfy a contract.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("document violates {contract}: {}", errors.join("; "))]
pub struct ContractViolation {
    pub contract: Contract,
    /// One entry per violation, each naming the JSON Pointer of the offending value.
    pub errors: Vec<String>,
}

/// Untrusted text rejected by [`Contract::parse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    #[error("output for {contract} is not JSON: {reason}")]
    NotJson { contract: Contract, reason: String },
    #[error(transparent)]
    Violation(#[from] ContractViolation),
}

struct Compiled {
    id: &'static str,
    validator: jsonschema::Validator,
}

fn compiled(contract: Contract) -> &'static Compiled {
    static COMPILED: LazyLock<Vec<Compiled>> = LazyLock::new(|| {
        Contract::ALL
            .iter()
            .map(|contract| {
                let schema: Value = serde_json::from_str(contract.schema_source())
                    .expect("embedded contract schema is JSON");
                let validator =
                    jsonschema::validator_for(&schema).expect("embedded contract schema compiles");
                let id = schema["$id"]
                    .as_str()
                    .expect("embedded contract schema has an $id")
                    .to_owned()
                    .leak();
                Compiled { id, validator }
            })
            .collect()
    });
    let index = Contract::ALL
        .iter()
        .position(|candidate| *candidate == contract)
        .expect("every contract is listed in Contract::ALL");
    &COMPILED[index]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn every_schema_is_valid_draft_2020_12_with_a_versioned_kiss_id() {
        for contract in Contract::ALL {
            let schema: Value = serde_json::from_str(contract.schema_source()).unwrap();
            jsonschema::meta::validate(&schema)
                .unwrap_or_else(|error| panic!("{contract:?} is not a valid schema: {error}"));
            assert_eq!(schema["$schema"], "https://json-schema.org/draft/2020-12/schema");
            let id = contract.schema_id();
            assert!(id.starts_with("kiss://schemas/") && id.ends_with("/v1"), "{id}");
        }
    }

    #[test]
    fn violations_name_the_offending_location() {
        let document = json!({
            "id": "node_000001",
            "type": "plan",
            "payload": {},
            "created_at": "2026-09-22T19:19:34.123Z",
            "updated_at": "2026-09-22T19:19:34.123Z"
        });
        let violation = Contract::DagNode.validate(&document).unwrap_err();
        assert_eq!(violation.contract, Contract::DagNode);
        assert!(violation.errors[0].starts_with("at /type:"), "{violation}");
        assert!(violation.to_string().contains("kiss://schemas/dag/v1"));
    }
}
