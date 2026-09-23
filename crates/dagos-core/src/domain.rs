//! Domain layer: typed DAGOS records and the Rust mirror of the machine contracts.
//!
//! Nodes, edges, runs, events, Jev classifications, inference IR, and inference responses are
//! defined here. This layer depends on nothing else in the crate and knows nothing about
//! persistence, providers, or transports.
//!
//! Serialization is deterministic: struct fields serialize in declaration order and JSON object
//! keys (payloads) in sorted order, so identical values always produce identical JSON.

mod context;
mod conversation;
mod dag;
mod event;
mod ids;
mod ir;
mod jev;
mod project;
mod response;
mod run;
mod schema;
mod time;

pub(crate) use dag::closed_enum;

pub use context::{Classification, ContextMember, ContextSource};
pub use conversation::{ConversationTurn, Role};
pub use dag::{DagEdge, DagNode, EdgeType, NodeType, Payload, UnknownVariant};
pub use event::{Event, EventData};
pub use ids::{
    ConversationId, EdgeId, EventId, IdError, IdGenerator, NodeId, ProjectId, RandomIds, RunId,
    SequentialIds,
};
pub use ir::{InferenceIr, IrContextItem, IrEvent, IrEventType, IrRelation, IrTask, IrTool};
pub use jev::{ContextClassification, JevCandidate, JevEdge, JevRequest, NodeClassification};
pub use project::{Conversation, Project};
pub use response::{
    Emission, EmissionRef, EmissionRefError, Endpoint, InferenceResponse, Presentation, ToolCall,
};
pub use run::{ErrorCode, IdentityError, ModelId, ProviderId, Run, RunConfig, RunStatus};
pub use schema::{InferenceIrSchema, InferenceResponseSchema, JevContextSchema, JevRequestSchema};
pub use time::{Clock, SteppingClock, SystemClock, Timestamp, TimestampError};
