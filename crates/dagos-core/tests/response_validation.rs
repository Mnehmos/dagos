//! Structured response validation: fail-closed parsing of `kiss.inference-response.v1` and checks
//! against the IR the response answers.

mod common;

use common::payload;
use dagos_core::contracts::{Contract, ContractError};
use dagos_core::domain::{
    EmissionRef, Endpoint, InferenceIr, InferenceIrSchema, InferenceResponse, IrContextItem,
    IrTask, ModelId, NodeId, NodeType,
};
use dagos_core::provider::{CollectDeltas, FakeProvider, InferenceProvider, InferenceRequest};
use dagos_core::response::{ResponseError, validate_response};
use serde_json::{Value, json};

fn node_id(n: u32) -> NodeId {
    NodeId::parse(format!("node_{n:06}")).unwrap()
}

/// IR showing one context node (node_000001) and the task (node_000002).
fn ir() -> InferenceIr {
    InferenceIr {
        schema: InferenceIrSchema,
        system_prompt: String::new(),
        task: IrTask { node_id: node_id(2), message: "Record the decision".into() },
        context: vec![IrContextItem {
            node_id: node_id(1),
            node_type: NodeType::Task,
            payload: payload(json!({"title": "Pick storage"})),
            relations: vec![],
        }],
        recent_events: vec![],
        tools: vec![],
        tool_results: vec![],
        recalled: Vec::new(),
    }
}

fn response(emissions: Value) -> Value {
    json!({
        "schema": "kiss.inference-response.v1",
        "presentation": {"prose": "Recorded the storage decision."},
        "emissions": emissions
    })
}

fn validate(document: &Value) -> Result<InferenceResponse, ResponseError> {
    validate_response(&document.to_string(), &ir()).map(|valid| valid.into_response())
}

#[test]
fn a_valid_response_separates_prose_from_emissions() {
    let document = json!({
        "schema": "kiss.inference-response.v1",
        "presentation": {"prose": "Recorded the storage decision."},
        "emissions": [
            {"kind": "node", "ref": "choice", "type": "decision", "payload": {"text": "SQLite"}},
            {"kind": "edge", "from": "node_000001", "to": "choice", "type": "depends_on"},
            {"kind": "edge", "from": "choice", "to": "node_000002", "type": "observed_from"}
        ],
        "tool_calls": [{"name": "read_file", "arguments": {"path": "Cargo.toml"}}],
        "metadata": {"usage": {"output_tokens": 42}}
    });
    let validated = validate_response(&document.to_string(), &ir()).unwrap();
    assert_eq!(validated.prose(), "Recorded the storage decision.");
    assert_eq!(validated.emissions().len(), 3);
    let response = validated.into_response();
    assert_eq!(response.tool_calls[0].name, "read_file");
    assert_eq!(response.metadata.unwrap()["usage"]["output_tokens"], 42);
    // The typed mirror re-serializes to the same document.
    assert_eq!(serde_json::to_value(validate(&document).unwrap()).unwrap(), document);
}

#[test]
fn malformed_json_is_rejected() {
    for raw in ["", "{", "Sure! Here is the JSON:", r#"{"schema":"kiss.inference-response.v1""#] {
        let result = validate_response(raw, &ir());
        assert!(
            matches!(result, Err(ResponseError::Contract(ContractError::NotJson { .. }))),
            "{raw:?}"
        );
    }
}

#[test]
fn schema_violations_are_rejected_by_contract_and_mirror() {
    let node = |extra: Value| {
        let mut emission = json!({"kind": "node", "ref": "a", "type": "decision", "payload": {}});
        for (key, value) in extra.as_object().unwrap() {
            emission[key] = value.clone();
        }
        response(json!([emission]))
    };
    let mut missing_presentation = response(json!([]));
    missing_presentation.as_object_mut().unwrap().remove("presentation");
    let mut prose_not_text = response(json!([]));
    prose_not_text["presentation"]["prose"] = json!(["not", "a", "string"]);
    let mut extra_presentation = response(json!([]));
    extra_presentation["presentation"]["markdown"] = json!("**hi**");
    let mut plan = response(json!([]));
    plan["plan"] = json!(["step 1"]);
    let mut wrong_version = response(json!([]));
    wrong_version["schema"] = json!("kiss.inference-response.v2");
    let mut bad_tool_call = response(json!([]));
    bad_tool_call["tool_calls"] = json!([{"name": "shell"}]);

    for document in [
        missing_presentation,
        prose_not_text,
        extra_presentation,
        plan,
        wrong_version,
        bad_tool_call,
        response(json!([{"kind": "plan", "steps": []}])),
        response(json!([{"kind": "edge", "from": "a", "to": "b"}])),
        node(json!({"ref": "node_1"})),
        node(json!({"ref": "1st"})),
        node(json!({"type": "plan"})),
        node(json!({"payload": "text"})),
        node(json!({"confidence": 0.9})),
    ] {
        assert!(
            matches!(
                validate(&document),
                Err(ResponseError::Contract(ContractError::Violation(_)))
            ),
            "accepted {document}"
        );
        assert!(
            serde_json::from_value::<InferenceResponse>(document.clone()).is_err(),
            "typed mirror accepted {document}"
        );
    }
}

#[test]
fn refs_must_be_unique() {
    let document = response(json!([
        {"kind": "node", "ref": "a", "type": "task", "payload": {}},
        {"kind": "node", "ref": "a", "type": "decision", "payload": {}}
    ]));
    assert_eq!(
        validate(&document),
        Err(ResponseError::DuplicateRef(EmissionRef::parse("a").unwrap()))
    );
}

#[test]
fn edges_may_only_touch_declared_refs_and_nodes_shown_in_the_ir() {
    let undeclared = response(json!([
        {"kind": "edge", "from": "ghost", "to": "node_000001", "type": "related_to"}
    ]));
    assert_eq!(
        validate(&undeclared),
        Err(ResponseError::UnknownEndpoint(Endpoint::Ref(EmissionRef::parse("ghost").unwrap())))
    );

    // node_000003 may exist in the DAG, but the model was never shown it.
    let unseen = response(json!([
        {"kind": "node", "ref": "a", "type": "task", "payload": {}},
        {"kind": "edge", "from": "a", "to": "node_000003", "type": "depends_on"}
    ]));
    assert_eq!(validate(&unseen), Err(ResponseError::UnknownEndpoint(Endpoint::Node(node_id(3)))));

    let shown = response(json!([
        {"kind": "edge", "from": "node_000001", "to": "node_000002", "type": "related_to"}
    ]));
    assert!(validate(&shown).is_ok(), "edges between nodes shown in the IR are allowed");
}

#[tokio::test]
async fn streamed_prose_does_not_make_invalid_output_usable() {
    let ir = ir();
    for model in ["fake-malformed", "fake-invalid-schema", "fake-dangling-edge"] {
        let model_id = ModelId::parse(model).unwrap();
        let mut deltas = CollectDeltas::default();
        let raw = FakeProvider::new()
            .infer(InferenceRequest { model_id: &model_id, ir: &ir }, &mut deltas)
            .await
            .unwrap();
        assert!(!deltas.0.is_empty(), "{model} streamed prose");
        assert!(validate_response(&raw, &ir).is_err(), "{model} must be rejected");
    }
    let model_id = ModelId::parse("fake-echo").unwrap();
    let raw = FakeProvider::new()
        .infer(InferenceRequest { model_id: &model_id, ir: &ir }, &mut CollectDeltas::default())
        .await
        .unwrap();
    assert!(validate_response(&raw, &ir).is_ok());
}

#[test]
fn the_response_schema_is_the_published_contract() {
    let schema: Value = serde_json::from_str(Contract::InferenceResponse.schema_source()).unwrap();
    assert_eq!(schema["$id"], "kiss://schemas/inference-response/v1");
    assert_eq!(schema["required"], json!(["schema", "presentation", "emissions"]));
}
