//! Runtime layer: the run lifecycle.
//!
//! A run records the user message, asks Jev to classify context, projects active context,
//! compiles IR, invokes the selected provider, validates the structured response, applies valid
//! emissions to the DAG, and records every transition as an ordered event.
