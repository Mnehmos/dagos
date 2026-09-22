//! Provider layer: the common inference adapter interface and the deterministic fake provider.
//!
//! Providers are interchangeable inference endpoints. They receive versioned IR and stream
//! presentation deltas plus a raw final response. They cannot see the store, and their output is
//! never trusted until the [`crate::response`] layer validates it.
