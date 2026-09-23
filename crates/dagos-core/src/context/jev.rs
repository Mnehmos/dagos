//! The Jev classifier adapter interface.

use async_trait::async_trait;

use super::recall::{RecallChunk, lexical_relevance};
use crate::domain::JevRequest;

/// A Jev endpoint could not produce any output (e.g. it was unreachable). Output that *was*
/// produced but violates the contract is a different failure, detected when DAGOS validates it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Jev classifier unavailable: {0}")]
pub struct JevError(pub String);

/// A Jev classifier endpoint.
///
/// Implementations only classify: they receive a `kiss.jev-request.v1` request and return raw
/// `kiss.jev-context.v1` text. They never see the store, choose providers, plan, or act, and their
/// output is untrusted until DAGOS validates it against the contract and the request.
#[async_trait]
pub trait JevClassifier: Send + Sync {
    /// Stable identity of this classifier, recorded with every classification request.
    fn id(&self) -> &str;

    /// Classifies context membership for `request`, returning raw output text.
    async fn classify(&self, request: &JevRequest) -> Result<String, JevError>;

    /// For each chunk, the probability that it holds information relevant to `query`, in chunk
    /// order. Used by recall to find earlier conversation turns. The default judges by shared
    /// words, without a model.
    async fn relevance(&self, query: &str, chunks: &[RecallChunk]) -> Result<Vec<f64>, JevError> {
        Ok(lexical_relevance(query, chunks))
    }

    /// Whether [`JevClassifier::relevance`] is judged by a model rather than by shared words.
    fn judges_relevance(&self) -> bool {
        false
    }
}
