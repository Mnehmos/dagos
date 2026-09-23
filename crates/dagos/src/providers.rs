//! Which inference providers this process offers, and how to reach them.
//!
//! Provider connection details are transport configuration, kept out of the core and out of the
//! database: the fake provider is always available, well-known endpoints are enabled by their
//! standard API-key environment variables, and `.dagos/providers.json` can add or override
//! OpenAI-compatible endpoints. API keys are only ever read from the environment.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dagos_core::context::{FakeJev, JevClassifier};
use dagos_core::domain::{ModelId, ProviderId};
use dagos_core::provider::{FakeProvider, InferenceProvider};
use dagos_openai::{OpenAiCompatible, OpenAiCompatibleConfig, OpenAiCompatibleJev};
use serde::Deserialize;

/// Pause between fake-provider deltas, so streaming is visible to people.
const FAKE_DELTA_DELAY: Duration = Duration::from_millis(20);

/// The optional provider configuration file inside the DAGOS directory.
pub const PROVIDERS_FILE: &str = "providers.json";

/// `providers.json`: endpoints to offer in addition to the built-in ones.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProvidersFile {
    #[serde(default)]
    providers: Vec<ProviderEntry>,
    /// Classify context with a model on one of the providers instead of the offline fake Jev.
    #[serde(default)]
    jev: Option<JevEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JevEntry {
    provider: ProviderId,
    model: ModelId,
}

/// The providers and the Jev classifier this process uses.
pub struct Loaded {
    pub providers: Vec<Arc<dyn InferenceProvider>>,
    pub jev: Arc<dyn JevClassifier>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderEntry {
    id: ProviderId,
    kind: ProviderKind,
    base_url: String,
    /// Name of the environment variable holding the API key; omit for keyless endpoints.
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default)]
    models: Vec<ModelId>,
    #[serde(default = "default_json_mode")]
    json_mode: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ProviderKind {
    OpenaiCompatible,
}

fn default_json_mode() -> bool {
    true
}

/// Endpoints enabled automatically when their standard API-key variable is set.
const PRESETS: &[(&str, &str, &str)] = &[
    ("openrouter", "https://openrouter.ai/api/v1", "OPENROUTER_API_KEY"),
    ("openai", "https://api.openai.com/v1", "OPENAI_API_KEY"),
];

/// Builds the providers to register, reporting configuration problems as readable errors.
/// `warn` receives non-fatal notes (e.g. a configured key variable that is not set).
pub fn load(
    dagos_dir: &Path,
    env: impl Fn(&str) -> Option<String>,
    mut warn: impl FnMut(String),
) -> Result<Loaded, String> {
    let mut jev_entry: Option<JevEntry> = None;
    let mut configs: Vec<OpenAiCompatibleConfig> = Vec::new();
    for (id, base_url, key_env) in PRESETS {
        if let Some(key) = env(key_env) {
            configs.push(OpenAiCompatibleConfig {
                id: ProviderId::parse(*id).expect("preset ids are valid"),
                base_url: (*base_url).to_owned(),
                api_key: Some(key),
                models: Vec::new(),
                json_mode: true,
            });
        }
    }

    let path = dagos_dir.join(PROVIDERS_FILE);
    if path.exists() {
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let file: ProvidersFile = serde_json::from_str(&text)
            .map_err(|error| format!("invalid {}: {error}", path.display()))?;
        jev_entry = file.jev;
        for entry in file.providers {
            let ProviderKind::OpenaiCompatible = entry.kind;
            let api_key = entry.api_key_env.as_deref().and_then(|name| {
                let key = env(name);
                if key.is_none() {
                    warn(format!(
                        "provider `{}`: environment variable {name} is not set",
                        entry.id
                    ));
                }
                key
            });
            configs.retain(|config| config.id != entry.id);
            configs.push(OpenAiCompatibleConfig {
                id: entry.id,
                base_url: entry.base_url,
                api_key,
                models: entry.models,
                json_mode: entry.json_mode,
            });
        }
    }

    let mut chats: Vec<OpenAiCompatible> = Vec::new();
    let mut providers: Vec<Arc<dyn InferenceProvider>> =
        vec![Arc::new(FakeProvider::new().with_delta_delay(FAKE_DELTA_DELAY))];
    for config in configs {
        if config.id.as_str() == FakeProvider::ID {
            return Err("provider id `fake` is reserved for the built-in fake provider".into());
        }
        providers.push(Arc::new(OpenAiCompatible::new(config.clone())));
        chats.push(OpenAiCompatible::new(config));
    }

    // DAGOS_JEV_PROVIDER and DAGOS_JEV_MODEL override the file.
    if let (Some(provider), Some(model)) = (env("DAGOS_JEV_PROVIDER"), env("DAGOS_JEV_MODEL")) {
        let provider =
            ProviderId::parse(provider).map_err(|error| format!("DAGOS_JEV_PROVIDER: {error}"))?;
        let model = ModelId::parse(model).map_err(|error| format!("DAGOS_JEV_MODEL: {error}"))?;
        jev_entry = Some(JevEntry { provider, model });
    }
    let jev: Arc<dyn JevClassifier> = match jev_entry {
        None => Arc::new(FakeJev::new()),
        Some(entry) => {
            let chat = chats
                .into_iter()
                .find(|chat| chat.config().id == entry.provider)
                .ok_or_else(|| {
                    format!("Jev provider `{}` is not configured (set its API key or add it to {PROVIDERS_FILE})", entry.provider)
                })?;
            Arc::new(OpenAiCompatibleJev::new(chat, entry.model))
        }
    };
    Ok(Loaded { providers, jev })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn ids(providers: &[Arc<dyn InferenceProvider>]) -> Vec<String> {
        providers.iter().map(|provider| provider.id().to_string()).collect()
    }

    #[test]
    fn the_fake_provider_is_always_available_and_presets_follow_their_key_variables() {
        let dir = tempfile::tempdir().unwrap();
        let none = load(dir.path(), |_| None, |_| {}).unwrap().providers;
        assert_eq!(ids(&none), ["fake"]);

        let env: HashMap<&str, &str> = HashMap::from([("OPENROUTER_API_KEY", "k")]);
        let with_key = load(dir.path(), |name| env.get(name).map(|v| v.to_string()), |_| {});
        assert_eq!(ids(&with_key.unwrap().providers), ["fake", "openrouter"]);
    }

    #[test]
    fn providers_file_adds_and_overrides_endpoints_without_storing_keys() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(PROVIDERS_FILE),
            r#"{"providers": [
                {"id": "ollama", "kind": "openai-compatible", "base_url": "http://localhost:11434/v1",
                 "models": ["qwen2.5-coder:7b"], "json_mode": true},
                {"id": "openrouter", "kind": "openai-compatible", "base_url": "https://example.test/v1",
                 "api_key_env": "MISSING_KEY"}
            ]}"#,
        )
        .unwrap();
        let env: HashMap<&str, &str> = HashMap::from([("OPENROUTER_API_KEY", "k")]);
        let mut warnings = Vec::new();
        let providers =
            load(dir.path(), |name| env.get(name).map(|v| v.to_string()), |w| warnings.push(w))
                .unwrap()
                .providers;
        assert_eq!(ids(&providers), ["fake", "ollama", "openrouter"]);
        assert_eq!(providers[1].suggested_models()[0].as_str(), "qwen2.5-coder:7b");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("MISSING_KEY"));
    }

    #[test]
    fn invalid_configuration_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PROVIDERS_FILE);
        for text in [
            "not json",
            r#"{"providers": [{"id": "x", "kind": "anthropic", "base_url": "u"}]}"#,
            r#"{"providers": [{"id": "Bad Id", "kind": "openai-compatible", "base_url": "u"}]}"#,
            r#"{"providers": [{"id": "fake", "kind": "openai-compatible", "base_url": "u"}]}"#,
            r#"{"providers": [{"id": "x", "kind": "openai-compatible", "base_url": "u", "key": "sk"}]}"#,
        ] {
            std::fs::write(&path, text).unwrap();
            assert!(load(dir.path(), |_| None, |_| {}).is_err(), "accepted {text}");
        }
    }

    #[test]
    fn jev_defaults_to_the_offline_fake_and_can_use_a_configured_provider() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path(), |_| None, |_| {}).unwrap().jev.id(), "fake-jev");

        let env: HashMap<&str, &str> = HashMap::from([
            ("OPENROUTER_API_KEY", "k"),
            ("DAGOS_JEV_PROVIDER", "openrouter"),
            ("DAGOS_JEV_MODEL", "google/gemini-2.5-flash"),
        ]);
        let loaded = load(dir.path(), |name| env.get(name).map(|v| v.to_string()), |_| {}).unwrap();
        assert_eq!(loaded.jev.id(), "openrouter-jev:google/gemini-2.5-flash");

        std::fs::write(
            dir.path().join(PROVIDERS_FILE),
            r#"{"jev": {"provider": "openrouter", "model": "some/model"}}"#,
        )
        .unwrap();
        let error = load(dir.path(), |_| None, |_| {}).err().unwrap();
        assert!(error.contains("Jev provider `openrouter` is not configured"), "{error}");
    }
}
