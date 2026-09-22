//! DAGOS core runtime.
//!
//! DAGOS is a minimal DAG operating system for LLM coding workflows. The canonical pipeline is:
//!
//! ```text
//! user message → run → Jev classification → active context → versioned IR
//!   → inference provider → structured response → validated DAG emissions → event history
//! ```
//!
//! Layers, innermost first. Each layer may only depend on the layers listed for it; the
//! `boundaries` integration test enforces this.
//!
//! | layer        | responsibility                                               | may use                         |
//! |--------------|--------------------------------------------------------------|---------------------------------|
//! | [`domain`]   | typed records: nodes, edges, runs, events, contracts' types  | —                               |
//! | [`contracts`]| versioned JSON Schemas and validation                        | —                               |
//! | [`store`]    | SQLite persistence and transactions                          | domain                          |
//! | [`context`]  | Jev classifier adapter and active-context projection         | domain, contracts, store        |
//! | [`ir`]       | deterministic compilation of active context into IR          | domain, contracts, store        |
//! | [`provider`] | common inference adapter interface and the fake provider     | domain                          |
//! | [`response`] | fail-closed validation of structured provider output         | domain, contracts               |
//! | [`runtime`]  | run lifecycle wiring the layers together                     | all of the above                |
//!
//! Invariants protected by these boundaries:
//! - Jev only classifies context membership; it never plans, routes, or acts.
//! - Removing a node from active context never deletes the durable DAG node.
//! - Providers receive versioned IR, never raw DAG records (the provider layer cannot see the store).
//! - Provider output is validated before any emission can mutate the DAG.

pub mod context;
pub mod contracts;
pub mod domain;
pub mod ir;
pub mod provider;
pub mod response;
pub mod runtime;
pub mod store;
