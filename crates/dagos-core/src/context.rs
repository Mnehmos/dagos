//! Context layer: the Jev classifier adapter and the active-context projection.
//!
//! Jev is only a classifier. It receives a machine-readable classification request and returns
//! node classifications. It never plans, routes, selects providers, executes tools, or acts.
//! Applying classifications changes active-context membership only; durable DAG nodes are never
//! modified or deleted by this layer.

mod fake_jev;
mod jev;
mod projection;
pub mod recall;

pub use fake_jev::{DEFAULT_CONVERSATION_WINDOW, FakeJev};
pub use jev::{JevClassifier, JevError};
pub use projection::{
    ClassificationError, ContextChanges, apply_classification, carry_context,
    classification_request, validate_classification,
};
