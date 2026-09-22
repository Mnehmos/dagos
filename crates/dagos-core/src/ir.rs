//! IR layer: deterministic compilation of a run's active context into versioned inference IR.
//!
//! IR is the only provider-facing contract. Identical state always compiles to identical IR, and
//! storage records never appear in it.
