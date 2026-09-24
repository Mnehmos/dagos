//! The DAGOS transport layer: workspace setup, provider configuration, read-only inspection
//! views, and the local HTTP server. The `dagos` binary is a thin command-line front end over it.

pub mod approvals;
pub mod guard;
pub mod inspect;
pub mod keys;
pub mod providers;
pub mod server;
pub mod workspace;
