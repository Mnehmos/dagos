//! Stopping a run: it fails with `cancelled` at its current step, and what it committed stays.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::memory_store;
use dagos_core::context::FakeJev;
use dagos_core::domain::{ErrorCode, EventData, ModelId, ProviderId, RunConfig, RunStatus};
use dagos_core::provider::FakeProvider;
use dagos_core::runtime::Runtime;

#[tokio::test]
async fn a_stopped_run_fails_as_cancelled_and_keeps_what_it_recorded() {
    let store = Arc::new(memory_store());
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let runtime = Arc::new(
        Runtime::new(store.clone(), Arc::new(FakeJev::new()))
            .with_provider(Arc::new(FakeProvider::new()))
            .with_inference_timeout(Duration::from_secs(600)),
    );
    let config = RunConfig {
        provider_id: ProviderId::parse("fake").unwrap(),
        // Streams a little, then never finishes.
        model_id: ModelId::parse("fake-timeout").unwrap(),
        system_prompt: String::new(),
    };
    let started = runtime.start(&project, "Take your time", &config).unwrap();
    let id = started.run.id.clone();
    assert!(!runtime.stop(&id), "not executing yet");
    let finishing = {
        let runtime = runtime.clone();
        tokio::spawn(async move { runtime.finish(started).await })
    };
    // Wait until inference is under way.
    loop {
        let events = store.transaction(|tx| tx.events(&id)).unwrap();
        if events.iter().any(|event| matches!(event.data, EventData::InferenceDelta { .. })) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(runtime.stop(&id));
    let run = finishing.await.unwrap().unwrap();
    assert_eq!((run.status, run.error_code), (RunStatus::Failed, Some(ErrorCode::Cancelled)));
    let events = store.transaction(|tx| tx.events(&id)).unwrap();
    let kinds: Vec<String> = events.iter().map(|event| event.data.event_type()).collect();
    assert!(kinds.contains(&"ir.compiled".to_owned()), "earlier steps stay: {kinds:?}");
    assert_eq!(kinds.last().map(String::as_str), Some("run.failed"));
    assert!(!runtime.stop(&id), "a finished run cannot be stopped");
}
