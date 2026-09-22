//! Response layer: fail-closed parsing and validation of `kiss.inference-response.v1`.
//!
//! Presentation prose is kept separate from canonical structured emissions. A
//! [`ValidatedResponse`] can only be produced by [`validate_response`], and it is the only form in
//! which a response's emissions may be applied to the DAG.

use std::collections::BTreeSet;

use crate::contracts::{Contract, ContractError};
use crate::domain::{Emission, EmissionRef, Endpoint, InferenceIr, InferenceResponse, NodeId};

/// Provider output that must not be used. Rejection is fail-closed: nothing from it is applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResponseError {
    #[error(transparent)]
    Contract(#[from] ContractError),
    #[error("emission ref `{0}` is declared more than once")]
    DuplicateRef(EmissionRef),
    #[error("edge endpoint `{0}` is neither a ref declared in this response nor a node in the IR")]
    UnknownEndpoint(Endpoint),
}

/// A response that passed validation against the contract and the IR it answers.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedResponse(InferenceResponse);

impl ValidatedResponse {
    pub fn response(&self) -> &InferenceResponse {
        &self.0
    }

    /// Prose for people. Never canonical state.
    pub fn prose(&self) -> &str {
        &self.0.presentation.prose
    }

    pub fn emissions(&self) -> &[Emission] {
        &self.0.emissions
    }

    pub fn into_response(self) -> InferenceResponse {
        self.0
    }
}

/// Validates raw provider output: it must be JSON satisfying `kiss.inference-response.v1`, every
/// node emission must declare a unique ref, and every edge endpoint must be a ref declared in the
/// response or a node that was in the IR. DAG rules that depend on live state (duplicates, cycles)
/// are enforced when the emissions are applied.
pub fn validate_response(raw: &str, ir: &InferenceIr) -> Result<ValidatedResponse, ResponseError> {
    let response: InferenceResponse = Contract::InferenceResponse.parse(raw)?;

    let mut refs = BTreeSet::new();
    for emission in &response.emissions {
        if let Emission::Node { reference, .. } = emission
            && !refs.insert(reference)
        {
            return Err(ResponseError::DuplicateRef(reference.clone()));
        }
    }
    let visible: BTreeSet<&NodeId> =
        ir.context.iter().map(|item| &item.node_id).chain([&ir.task.node_id]).collect();
    for emission in &response.emissions {
        if let Emission::Edge { from, to, .. } = emission {
            for endpoint in [from, to] {
                let known = match endpoint {
                    Endpoint::Ref(reference) => refs.contains(reference),
                    Endpoint::Node(node_id) => visible.contains(node_id),
                };
                if !known {
                    return Err(ResponseError::UnknownEndpoint(endpoint.clone()));
                }
            }
        }
    }
    Ok(ValidatedResponse(response))
}
