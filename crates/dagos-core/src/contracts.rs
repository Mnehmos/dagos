//! Versioned JSON Schema contracts (`specs/001-core-runtime/contracts`) and their validation.
//!
//! Every machine interface in DAGOS is versioned JSON. Documents crossing a boundary (Jev output,
//! compiled IR, provider responses) are validated against these schemas and rejected when invalid.
