//! The deterministic, offline fake provider.

use std::time::Duration;

use async_trait::async_trait;

use super::{DeltaSink, InferenceProvider, InferenceRequest, ProviderError};
use crate::domain::{
    EdgeType, Emission, EmissionRef, Endpoint, InferenceIr, InferenceResponse,
    InferenceResponseSchema, ModelId, NodeId, NodeType, Payload, Presentation, ProviderId,
    ToolCall, closed_enum,
};

closed_enum!(
    /// The fake provider's behaviours, selected by model ID.
    FakeModel, "fake model" {
        /// A valid response derived from the IR alone: prose, one observation, one edge.
        Echo => "fake-echo",
        /// Streams prose, then returns truncated, malformed JSON.
        Malformed => "fake-malformed",
        /// Returns JSON that violates the response schema.
        InvalidSchema => "fake-invalid-schema",
        /// Returns a schema-valid response with an edge to a node that is not in the IR.
        DanglingEdge => "fake-dangling-edge",
        /// Returns a schema-valid response whose edges form a cycle.
        Cycle => "fake-cycle",
        /// Streams one delta, then never finishes.
        Timeout => "fake-timeout",
        /// Fails like an unreachable endpoint.
        Error => "fake-error",
        /// Asks for one tool call, then reports its result. The message may name a listed tool
        /// and give JSON arguments (`ooda.read_file {"path": "README.md"}`); otherwise the first
        /// listed tool is called without arguments.
        Tool => "fake-tool",
    }
);

/// A deterministic provider that needs no network. Its output is a function of the IR and the
/// model ID alone, and deltas stream word by word in a fixed order.
#[derive(Debug, Clone)]
pub struct FakeProvider {
    id: ProviderId,
    delta_delay: Duration,
}

impl FakeProvider {
    pub const ID: &'static str = "fake";

    pub fn new() -> Self {
        Self {
            id: ProviderId::parse(Self::ID).expect("valid provider id"),
            delta_delay: Duration::ZERO,
        }
    }

    /// Pauses before each delta so streaming is visible to people watching a run.
    pub fn with_delta_delay(mut self, delay: Duration) -> Self {
        self.delta_delay = delay;
        self
    }

    /// The response `fake-echo` gives for `ir`.
    pub fn echo_response(ir: &InferenceIr) -> InferenceResponse {
        let size = ir.context.len();
        let prose = format!(
            "fake-echo received: \"{}\". The active context holds {size} node{}. Recorded one \
             observation linked to your message.",
            ir.task.message,
            if size == 1 { "" } else { "s" },
        );
        let mut payload = Payload::new();
        payload.insert("text".into(), format!("Observed request: {}", ir.task.message).into());
        payload.insert("context_size".into(), size.into());
        payload.insert("source".into(), "fake-echo".into());
        let observation = reference("observation");
        InferenceResponse {
            schema: InferenceResponseSchema,
            presentation: Presentation { prose },
            emissions: vec![
                Emission::Node {
                    reference: observation.clone(),
                    node_type: NodeType::Observation,
                    payload,
                },
                Emission::Edge {
                    from: Endpoint::Ref(observation),
                    to: Endpoint::Node(ir.task.node_id.clone()),
                    edge_type: EdgeType::ObservedFrom,
                },
            ],
            tool_calls: Vec::new(),
            metadata: None,
        }
    }

    async fn stream(&self, prose: &str, deltas: &mut dyn DeltaSink) {
        for word in prose.split_inclusive(' ') {
            if !self.delta_delay.is_zero() {
                tokio::time::sleep(self.delta_delay).await;
            }
            deltas.delta(word);
        }
    }
}

impl Default for FakeProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// The `fake-tool` response for `ir`: one tool call, then a report of what came back.
fn tool_response(ir: &InferenceIr) -> InferenceResponse {
    if let Some(result) = ir.tool_results.last() {
        let outcome = match (&result.output, &result.reason) {
            (_, Some(reason)) => format!("was denied: {reason}"),
            (Some(output), None) => {
                let text = output.to_string();
                let excerpt: String = text.chars().take(300).collect();
                format!("returned {} {excerpt}", result.status)
            }
            (None, None) => format!("ended {}", result.status),
        };
        return response(&format!("`{}` {outcome}", result.name), Vec::new());
    }
    let message = ir.task.message.trim();
    let named = ir.tools.iter().find(|tool| {
        message
            .strip_prefix(tool.name.as_str())
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    });
    let Some(tool) = named.or(ir.tools.first()) else {
        return response("No tools are available to this run.", Vec::new());
    };
    let arguments = named
        .and_then(|tool| serde_json::from_str::<Payload>(message[tool.name.len()..].trim()).ok())
        .unwrap_or_default();
    let mut calling = response(&format!("Calling `{}`.", tool.name), Vec::new());
    calling.tool_calls.push(ToolCall { name: tool.name.clone(), arguments });
    calling
}

fn reference(name: &str) -> EmissionRef {
    EmissionRef::parse(name).expect("valid emission ref")
}

fn to_json(response: &InferenceResponse) -> String {
    serde_json::to_string(response).expect("responses serialize")
}

fn response(prose: &str, emissions: Vec<Emission>) -> InferenceResponse {
    InferenceResponse {
        schema: InferenceResponseSchema,
        presentation: Presentation { prose: prose.to_owned() },
        emissions,
        tool_calls: Vec::new(),
        metadata: None,
    }
}

fn node(name: &str, node_type: NodeType) -> Emission {
    Emission::Node { reference: reference(name), node_type, payload: Payload::new() }
}

fn edge(from: Endpoint, to: Endpoint) -> Emission {
    Emission::Edge { from, to, edge_type: EdgeType::DependsOn }
}

#[async_trait]
impl InferenceProvider for FakeProvider {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn suggested_models(&self) -> Vec<ModelId> {
        FakeModel::ALL
            .iter()
            .map(|model| ModelId::parse(model.as_str()).expect("valid model id"))
            .collect()
    }

    async fn infer(
        &self,
        request: InferenceRequest<'_>,
        deltas: &mut dyn DeltaSink,
    ) -> Result<String, ProviderError> {
        let model: FakeModel = request
            .model_id
            .as_str()
            .parse()
            .map_err(|_| ProviderError::UnknownModel(request.model_id.clone()))?;
        match model {
            FakeModel::Echo => {
                let response = Self::echo_response(request.ir);
                self.stream(&response.presentation.prose, deltas).await;
                Ok(to_json(&response))
            }
            FakeModel::Tool => {
                let response = tool_response(request.ir);
                self.stream(&response.presentation.prose, deltas).await;
                Ok(to_json(&response))
            }
            FakeModel::Malformed => {
                let prose = "This response will be cut off mid-document.";
                self.stream(prose, deltas).await;
                Ok(format!(
                    r#"{{"schema":"kiss.inference-response.v1","presentation":{{"prose":"{prose}"#
                ))
            }
            FakeModel::InvalidSchema => {
                let prose = "Here is a plan instead of emissions.";
                self.stream(prose, deltas).await;
                Ok(serde_json::json!({
                    "schema": "kiss.inference-response.v1",
                    "presentation": {"prose": prose},
                    "emissions": [{"kind": "plan", "steps": ["rewrite everything"]}]
                })
                .to_string())
            }
            FakeModel::DanglingEdge => {
                let prose = "Linking to a node I was never shown.";
                self.stream(prose, deltas).await;
                let unseen = NodeId::parse("node_zzzzzz").expect("valid node id");
                let emissions = vec![
                    node("claim", NodeType::Decision),
                    edge(Endpoint::Ref(reference("claim")), Endpoint::Node(unseen)),
                ];
                Ok(to_json(&response(prose, emissions)))
            }
            FakeModel::Cycle => {
                let prose = "Two tasks that depend on each other.";
                self.stream(prose, deltas).await;
                let (first, second) = (reference("first"), reference("second"));
                let emissions = vec![
                    node("first", NodeType::Task),
                    node("second", NodeType::Task),
                    edge(Endpoint::Ref(first.clone()), Endpoint::Ref(second.clone())),
                    edge(Endpoint::Ref(second), Endpoint::Ref(first)),
                ];
                Ok(to_json(&response(prose, emissions)))
            }
            FakeModel::Timeout => {
                self.stream("Thinking... ", deltas).await;
                std::future::pending().await
            }
            FakeModel::Error => {
                Err(ProviderError::Failed("fake-error: simulated endpoint failure".into()))
            }
        }
    }
}
