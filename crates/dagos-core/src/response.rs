//! Response layer: fail-closed parsing and validation of `kiss.inference-response.v1`.
//!
//! Presentation prose is kept separate from canonical structured emissions. Only a validated final
//! response can produce emissions that mutate the DAG.
