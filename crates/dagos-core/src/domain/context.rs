//! Active-context membership: which durable nodes a run projects into inference, and why.

use serde::{Deserialize, Serialize};

use super::dag::closed_enum;
use super::ids::{NodeId, RunId};

closed_enum!(
    /// A Jev classification label for one node.
    Classification, "classification" {
        /// The node belongs in the run's active context.
        Active => "active",
        /// The node does not belong in the run's active context. It stays in the DAG.
        Inactive => "inactive",
    }
);

closed_enum!(
    /// Why a node is a member of a run's active context.
    ContextSource, "context source" {
        /// Carried over from the previous run's active context and not reclassified since.
        Carried => "carried",
        /// Classified `active` by Jev during this run.
        Jev => "jev",
    }
);

/// One node's membership in a run's active context.
///
/// Membership only references the node: removing it never touches the durable DAG. Members always
/// carry the `active` classification; `ordering` is the node's position in the compiled IR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextMember {
    pub run_id: RunId,
    pub node_id: NodeId,
    pub classification: Classification,
    pub ordering: u32,
    pub source: ContextSource,
}
