//! Context layer: the Jev classifier adapter and the active-context projection.
//!
//! Jev is only a classifier. It receives a machine-readable classification request and returns
//! node classifications. It never plans, routes, selects providers, executes tools, or acts.
//! Applying classifications changes active-context membership only; durable DAG nodes are never
//! modified or deleted by this layer.
