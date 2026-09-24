//! Which inference providers this process offers, how to reach them, and which Jev classifies.
//!
//! Provider connection details are transport configuration, kept out of the core and out of the
//! database. The fake provider is always available; well-known endpoints (presets) become
//! available once they have an API key, from their standard environment variable or saved in the
//! app ([`crate::keys`]); `.dagos/providers.json` adds or overrides OpenAI-compatible endpoints and
//! can name a model-backed Jev. Keys never enter `providers.json`, the database, or the IR.
//!
//! A model-backed Jev is optional: the offline policy classifier is the default and the fallback,
//! so runs work without a model Jev and improve with one.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dagos_core::context::{FakeJev, JevClassifier};
use dagos_core::domain::{ModelId, ProviderId};
use dagos_core::provider::{FakeProvider, InferenceProvider};
use dagos_openai::{
    DecisionsJev, OpenAiCompatible, OpenAiCompatibleConfig, OpenAiCompatibleJev, is_decisions_model,
};
use serde::{Deserialize, Serialize};

use crate::keys::{self, KeyStore};

/// Pause between fake-provider deltas, so streaming is visible to people.
const FAKE_DELTA_DELAY: Duration = Duration::from_millis(20);

/// The optional provider configuration file inside the DAGOS directory.
pub const PROVIDERS_FILE: &str = "providers.json";

/// A well-known endpoint, available as soon as it has an API key.
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
    /// The standard environment variable holding its key; it wins over a saved key.
    pub key_env: &'static str,
    /// Where people create and manage keys.
    pub key_url: &'static str,
    /// A path below the base URL that answers 2xx only for a valid key, when `/models` is public.
    pub key_check: Option<&'static str>,
}

pub const PRESETS: &[Preset] = &[
    Preset {
        id: "openrouter",
        name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        key_env: "OPENROUTER_API_KEY",
        key_url: "https://openrouter.ai/settings/keys",
        key_check: Some("/key"),
    },
    Preset {
        id: "openai",
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        key_env: "OPENAI_API_KEY",
        key_url: "https://platform.openai.com/api-keys",
        key_check: None,
    },
    Preset {
        id: "zai",
        name: "Z.ai",
        base_url: "https://api.z.ai/api/paas/v4",
        key_env: "ZAI_API_KEY",
        key_url: "https://z.ai/manage-apikey/apikey-list",
        key_check: None,
    },
];

fn preset(id: &ProviderId) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.id == id.as_str())
}

/// `providers.json`: endpoints to offer in addition to the built-in ones, and the Jev to use.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProvidersFile {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<ProviderEntry>,
    /// Classify context with a model on one of the providers instead of the offline policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jev: Option<JevEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JevEntry {
    pub provider: ProviderId,
    pub model: ModelId,
}

/// One OpenAI-compatible endpoint in `providers.json`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderEntry {
    pub id: ProviderId,
    pub kind: ProviderKind,
    pub base_url: String,
    /// Name of the environment variable holding the API key. Optional: a key saved in the app
    /// works too, and keyless endpoints (e.g. Ollama) need neither.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelId>,
    #[serde(default = "default_json_mode")]
    pub json_mode: bool,
    /// Offer tools through the endpoint's native tool calling (DAGOS falls back to its JSON
    /// protocol for a model that refuses them).
    #[serde(default = "default_json_mode")]
    pub native_tools: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    OpenaiCompatible,
}

fn default_json_mode() -> bool {
    true
}

/// Where a provider's key comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeySource {
    /// The environment variable `env_var`; it wins over a saved key.
    Env,
    /// Saved in the app.
    Saved,
    /// No key: a preset is unavailable until it gets one, a custom endpoint is called without.
    None,
    /// The built-in fake provider needs no key.
    NotNeeded,
}

/// A provider's key status. Never contains the key itself.
#[derive(Debug, Clone, Serialize)]
pub struct KeyStatus {
    pub source: KeySource,
    pub env_var: Option<String>,
    /// Whether a key is saved in the app (it may be shadowed by the environment).
    pub saved: bool,
    /// A short, useless-to-attackers hint of the key in use, e.g. `sk-o…7890`.
    pub hint: Option<String>,
    /// Where to create or manage keys, for presets.
    pub manage_url: Option<String>,
}

/// Where a provider is defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Builtin,
    Preset,
    Custom,
}

/// One provider as the settings screen shows it.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderSetting {
    pub id: ProviderId,
    pub name: String,
    pub origin: Origin,
    pub base_url: Option<String>,
    pub models: Vec<ModelId>,
    pub json_mode: bool,
    pub key: KeyStatus,
    /// Whether runs can use it now.
    pub available: bool,
}

/// Where the Jev choice comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JevSource {
    Default,
    File,
    Env,
}

/// The Jev as the settings screen shows it.
#[derive(Debug, Clone, Serialize)]
pub struct JevSetting {
    /// The configured model Jev, if any.
    pub provider: Option<ProviderId>,
    pub model: Option<ModelId>,
    pub source: JevSource,
    /// The classifier runs ask first, e.g. `openrouter-jev:<model>` or `fake-jev`.
    pub active: String,
    /// The offline classifier used whenever the model Jev fails or is rejected.
    pub fallback: String,
    /// Why the configured model Jev cannot be used right now.
    pub problem: Option<String>,
}

/// Everything the settings screen shows. Contains no keys.
#[derive(Debug, Clone, Serialize)]
pub struct Settings {
    pub providers: Vec<ProviderSetting>,
    pub jev: JevSetting,
    /// Where keys saved in the app live.
    pub keys_file: Option<String>,
}

/// An endpoint that can be checked from the settings screen.
#[derive(Clone)]
pub struct Endpoint {
    pub chat: OpenAiCompatible,
    pub key_check: Option<&'static str>,
}

/// The providers, the Jev, and the settings view this process uses.
pub struct Loaded {
    pub providers: Vec<Arc<dyn InferenceProvider>>,
    pub jev: Arc<dyn JevClassifier>,
    /// Classifies whenever `jev` fails or is rejected.
    pub jev_fallback: Arc<dyn JevClassifier>,
    pub settings: Settings,
    pub endpoints: BTreeMap<ProviderId, Endpoint>,
}

/// Reads `providers.json` in `dagos_dir`; an empty configuration if it does not exist.
pub fn read_file(dagos_dir: &Path) -> Result<ProvidersFile, String> {
    let path = dagos_dir.join(PROVIDERS_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|error| format!("invalid {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ProvidersFile::default()),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

fn write_file(dagos_dir: &Path, file: &ProvidersFile) -> Result<(), String> {
    let path = dagos_dir.join(PROVIDERS_FILE);
    let text = serde_json::to_string_pretty(file).expect("provider configuration serializes");
    std::fs::write(&path, format!("{text}\n"))
        .map_err(|error| format!("cannot save {}: {error}", path.display()))
}

/// Adds or replaces the custom endpoint `entry.id` in `providers.json`.
pub fn save_custom(dagos_dir: &Path, entry: ProviderEntry) -> Result<(), String> {
    if entry.id.as_str() == FakeProvider::ID {
        return Err("provider id `fake` is reserved for the built-in fake provider".into());
    }
    validate_base_url(&entry.base_url)?;
    if let Some(name) = &entry.api_key_env {
        let valid = !name.is_empty()
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            && !name.as_bytes()[0].is_ascii_digit();
        if !valid {
            return Err(format!("`{name}` is not an environment variable name"));
        }
    }
    let mut file = read_file(dagos_dir)?;
    match file.providers.iter_mut().find(|existing| existing.id == entry.id) {
        Some(existing) => *existing = entry,
        None => file.providers.push(entry),
    }
    write_file(dagos_dir, &file)
}

/// Removes the custom endpoint `id` from `providers.json`; returns whether it was there.
pub fn remove_custom(dagos_dir: &Path, id: &ProviderId) -> Result<bool, String> {
    let mut file = read_file(dagos_dir)?;
    let before = file.providers.len();
    file.providers.retain(|entry| &entry.id != id);
    let removed = file.providers.len() != before;
    if removed {
        write_file(dagos_dir, &file)?;
    }
    Ok(removed)
}

/// Sets (or, with `None`, clears) the model Jev in `providers.json`.
pub fn set_jev(dagos_dir: &Path, jev: Option<JevEntry>) -> Result<(), String> {
    let mut file = read_file(dagos_dir)?;
    file.jev = jev;
    write_file(dagos_dir, &file)
}

fn validate_base_url(url: &str) -> Result<(), String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"));
    match rest {
        Some(rest) if !rest.is_empty() && !url.chars().any(char::is_whitespace) => Ok(()),
        _ => Err(format!("`{url}` is not an http(s) URL")),
    }
}

/// Builds the providers, the Jev, and the settings view. Keys come from `env` first and then from
/// `keys`. `warn` receives non-fatal notes (e.g. a configured key variable that is not set).
pub fn load(
    dagos_dir: &Path,
    env: impl Fn(&str) -> Option<String>,
    keys: Option<&KeyStore>,
    mut warn: impl FnMut(String),
) -> Result<Loaded, String> {
    let saved = match keys.map(KeyStore::load).transpose() {
        Ok(saved) => saved.unwrap_or_default(),
        Err(error) => {
            warn(format!("ignoring saved API keys: {error}"));
            BTreeMap::new()
        }
    };
    let env = |name: &str| env(name).filter(|value| !value.trim().is_empty());
    let file = read_file(dagos_dir)?;

    let mut settings = vec![ProviderSetting {
        id: ProviderId::parse(FakeProvider::ID).expect("valid provider id"),
        name: "Offline fake".into(),
        origin: Origin::Builtin,
        base_url: None,
        models: FakeProvider::new().suggested_models(),
        json_mode: true,
        key: KeyStatus {
            source: KeySource::NotNeeded,
            env_var: None,
            saved: false,
            hint: None,
            manage_url: None,
        },
        available: true,
    }];
    let mut configs: Vec<(OpenAiCompatibleConfig, Option<&'static str>)> = Vec::new();

    // A key from `env_var`, else a saved one.
    let resolve = |id: &ProviderId, env_var: Option<&str>| {
        let from_env = env_var.and_then(&env).map(|key| (KeySource::Env, key));
        from_env.or_else(|| saved.get(id).map(|key| (KeySource::Saved, key.clone())))
    };
    let status =
        |id: &ProviderId, env_var: Option<&str>, key: &Option<(KeySource, String)>| KeyStatus {
            source: key.as_ref().map_or(KeySource::None, |(source, _)| *source),
            env_var: env_var.map(str::to_owned),
            saved: saved.contains_key(id),
            hint: key.as_ref().map(|(_, key)| keys::hint(key)),
            manage_url: preset(id).map(|preset| preset.key_url.to_owned()),
        };

    for preset in PRESETS {
        let id = ProviderId::parse(preset.id).expect("preset ids are valid");
        if file.providers.iter().any(|entry| entry.id == id) {
            continue; // overridden below, in the preset's place
        }
        let key = resolve(&id, Some(preset.key_env));
        settings.push(ProviderSetting {
            id: id.clone(),
            name: preset.name.into(),
            origin: Origin::Preset,
            base_url: Some(preset.base_url.into()),
            models: Vec::new(),
            json_mode: true,
            key: status(&id, Some(preset.key_env), &key),
            available: key.is_some(),
        });
        if let Some((_, key)) = key {
            let config = OpenAiCompatibleConfig {
                id,
                base_url: preset.base_url.to_owned(),
                api_key: Some(key),
                models: Vec::new(),
                json_mode: true,
                native_tools: true,
                model_windows: preset.id == "openrouter",
            };
            configs.push((config, preset.key_check));
        }
    }

    for entry in file.providers {
        let ProviderKind::OpenaiCompatible = entry.kind;
        if entry.id.as_str() == FakeProvider::ID {
            return Err("provider id `fake` is reserved for the built-in fake provider".into());
        }
        let key = resolve(&entry.id, entry.api_key_env.as_deref());
        if let (Some(name), None) = (&entry.api_key_env, &key) {
            warn(format!(
                "provider `{}`: environment variable {name} is not set and no key is saved",
                entry.id
            ));
        }
        let setting = ProviderSetting {
            id: entry.id.clone(),
            name: preset(&entry.id).map_or_else(|| entry.id.to_string(), |p| p.name.to_owned()),
            origin: Origin::Custom,
            base_url: Some(entry.base_url.clone()),
            models: entry.models.clone(),
            json_mode: entry.json_mode,
            key: status(&entry.id, entry.api_key_env.as_deref(), &key),
            available: true,
        };
        settings.push(setting);
        let config = OpenAiCompatibleConfig {
            id: entry.id,
            base_url: entry.base_url,
            api_key: key.map(|(_, key)| key),
            models: entry.models,
            json_mode: entry.json_mode,
            native_tools: entry.native_tools,
            model_windows: false,
        };
        configs.push((config, None));
    }
    // The fake provider, then presets (overridden or not) in preset order, then custom endpoints.
    let rank = |id: &ProviderId| match PRESETS.iter().position(|preset| preset.id == id.as_str()) {
        _ if id.as_str() == FakeProvider::ID => 0,
        Some(index) => 1 + index,
        None => 1 + PRESETS.len(),
    };
    settings.sort_by_key(|setting| rank(&setting.id));
    configs.sort_by_key(|(config, _)| rank(&config.id));

    let mut endpoints = BTreeMap::new();
    let mut providers: Vec<Arc<dyn InferenceProvider>> =
        vec![Arc::new(FakeProvider::new().with_delta_delay(FAKE_DELTA_DELAY))];
    for (config, key_check) in configs {
        let chat = OpenAiCompatible::new(config);
        providers.push(Arc::new(chat.clone()));
        endpoints.insert(chat.config().id.clone(), Endpoint { chat, key_check });
    }

    // DAGOS_JEV_PROVIDER and DAGOS_JEV_MODEL override the file.
    let mut jev_entry = file.jev.map(|entry| (entry, JevSource::File));
    if let (Some(provider), Some(model)) = (env("DAGOS_JEV_PROVIDER"), env("DAGOS_JEV_MODEL")) {
        let provider =
            ProviderId::parse(provider).map_err(|error| format!("DAGOS_JEV_PROVIDER: {error}"))?;
        let model = ModelId::parse(model).map_err(|error| format!("DAGOS_JEV_MODEL: {error}"))?;
        jev_entry = Some((JevEntry { provider, model }, JevSource::Env));
    }
    let fallback: Arc<dyn JevClassifier> = Arc::new(FakeJev::new());
    let (jev, jev_setting) = match jev_entry {
        None => (fallback.clone(), jev_setting(None, JevSource::Default, fallback.id(), None)),
        Some((entry, source)) => match endpoints.get(&entry.provider) {
            Some(endpoint) => {
                // Decisions models (TypeSafe's Jev) use the Decisions API; others chat.
                let (chat, model) = (endpoint.chat.clone(), entry.model.clone());
                let jev: Arc<dyn JevClassifier> = if is_decisions_model(&model) {
                    Arc::new(DecisionsJev::new(chat, model))
                } else {
                    Arc::new(OpenAiCompatibleJev::new(chat, model))
                };
                let setting = jev_setting(Some(entry), source, jev.id(), None);
                (jev, setting)
            }
            None => {
                let problem = format!(
                    "Jev provider `{}` is not available (it needs an API key or is not \
                     configured); classifying with the offline policy instead",
                    entry.provider
                );
                warn(problem.clone());
                (fallback.clone(), jev_setting(Some(entry), source, fallback.id(), Some(problem)))
            }
        },
    };
    let settings = Settings {
        providers: settings,
        jev: JevSetting { fallback: fallback.id().to_owned(), ..jev_setting },
        keys_file: keys.map(|keys| keys.path().display().to_string()),
    };
    Ok(Loaded { providers, jev, jev_fallback: fallback, settings, endpoints })
}

fn jev_setting(
    entry: Option<JevEntry>,
    source: JevSource,
    active: &str,
    problem: Option<String>,
) -> JevSetting {
    let (provider, model) = entry.map(|entry| (entry.provider, entry.model)).unzip();
    JevSetting {
        provider,
        model,
        source,
        active: active.to_owned(),
        fallback: String::new(),
        problem,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn ids(providers: &[Arc<dyn InferenceProvider>]) -> Vec<String> {
        providers.iter().map(|provider| provider.id().to_string()).collect()
    }

    fn id(value: &str) -> ProviderId {
        ProviderId::parse(value).unwrap()
    }

    fn env_of(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<&str, &str> = pairs.iter().copied().collect();
        move |name| map.get(name).map(|value| value.to_string())
    }

    #[test]
    fn presets_become_available_with_an_environment_or_saved_key() {
        let dir = tempfile::tempdir().unwrap();
        let none = load(dir.path(), |_| None, None, |_| {}).unwrap();
        assert_eq!(ids(&none.providers), ["fake"]);
        let listed: Vec<_> = none.settings.providers.iter().map(|p| p.id.to_string()).collect();
        assert_eq!(listed, ["fake", "openrouter", "openai", "zai"], "presets are always listed");
        assert!(none.settings.providers[1..].iter().all(|p| !p.available));

        let keys = KeyStore::at(dir.path().join("keys.json"));
        keys.set(&id("zai"), "zai-saved-key-0001").unwrap();
        keys.set(&id("openrouter"), "sk-or-saved-0002").unwrap();
        let env = env_of(&[("OPENROUTER_API_KEY", "sk-or-from-env-0003")]);
        let loaded = load(dir.path(), env, Some(&keys), |_| {}).unwrap();
        assert_eq!(ids(&loaded.providers), ["fake", "openrouter", "zai"]);
        let openrouter = &loaded.settings.providers[1].key;
        assert_eq!((openrouter.source, openrouter.saved), (KeySource::Env, true), "env wins");
        assert_eq!(openrouter.hint.as_deref(), Some("sk-o…0003"));
        assert_eq!(
            loaded.endpoints[&id("openrouter")].chat.config().api_key.as_deref(),
            Some("sk-or-from-env-0003")
        );
        assert_eq!(
            loaded.endpoints[&id("zai")].chat.config().api_key.as_deref(),
            Some("zai-saved-key-0001")
        );
        assert_eq!(loaded.settings.providers[3].key.source, KeySource::Saved);

        let settings = serde_json::to_string(&loaded.settings).unwrap();
        assert!(!settings.contains("saved-key") && !settings.contains("from-env"), "{settings}");
    }

    #[test]
    fn providers_file_adds_and_overrides_endpoints_and_can_be_edited() {
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
        let mut warnings = Vec::new();
        let loaded = load(dir.path(), |_| None, None, |w| warnings.push(w)).unwrap();
        assert_eq!(ids(&loaded.providers), ["fake", "openrouter", "ollama"]);
        assert_eq!(loaded.providers[2].suggested_models()[0].as_str(), "qwen2.5-coder:7b");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("MISSING_KEY"));
        assert_eq!(loaded.settings.providers[1].origin, Origin::Custom);

        let entry = ProviderEntry {
            id: id("lmstudio"),
            kind: ProviderKind::OpenaiCompatible,
            base_url: "http://localhost:1234/v1".into(),
            api_key_env: None,
            models: vec![ModelId::parse("local").unwrap()],
            json_mode: false,
            native_tools: true,
        };
        save_custom(dir.path(), entry.clone()).unwrap();
        save_custom(dir.path(), entry).unwrap();
        assert!(remove_custom(dir.path(), &id("ollama")).unwrap());
        assert!(!remove_custom(dir.path(), &id("ollama")).unwrap());
        let loaded = load(dir.path(), |_| None, None, |_| {}).unwrap();
        assert_eq!(ids(&loaded.providers), ["fake", "openrouter", "lmstudio"]);

        let bad_url = ProviderEntry {
            base_url: "ftp://x".into(),
            ..read_file(dir.path()).unwrap().providers[1].clone()
        };
        assert!(save_custom(dir.path(), bad_url).is_err());
        let bad_env = ProviderEntry {
            api_key_env: Some("1 BAD".into()),
            ..read_file(dir.path()).unwrap().providers[1].clone()
        };
        assert!(save_custom(dir.path(), bad_env).is_err());
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
            assert!(load(dir.path(), |_| None, None, |_| {}).is_err(), "accepted {text}");
        }
    }

    #[test]
    fn a_corrupt_keys_file_is_ignored_with_a_warning_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let keys = KeyStore::at(dir.path().join("keys.json"));
        std::fs::write(keys.path(), "{corrupt").unwrap();
        let mut warnings = Vec::new();
        load(dir.path(), |_| None, Some(&keys), |w| warnings.push(w)).unwrap();
        assert!(warnings[0].contains("ignoring saved API keys"), "{warnings:?}");
        assert!(keys.set(&id("openrouter"), "sk-or-new-key").is_err());
        assert_eq!(std::fs::read_to_string(keys.path()).unwrap(), "{corrupt");
    }

    #[test]
    fn jev_is_optional_uses_a_configured_model_and_falls_back_when_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let offline = load(dir.path(), |_| None, None, |_| {}).unwrap();
        assert_eq!(offline.jev.id(), "fake-jev");
        assert_eq!(offline.settings.jev.source, JevSource::Default);
        assert_eq!(offline.jev_fallback.id(), "fake-jev");

        let env = env_of(&[
            ("OPENROUTER_API_KEY", "k"),
            ("DAGOS_JEV_PROVIDER", "openrouter"),
            ("DAGOS_JEV_MODEL", "google/gemini-2.5-flash"),
        ]);
        let loaded = load(dir.path(), env, None, |_| {}).unwrap();
        assert_eq!(loaded.jev.id(), "openrouter-jev:google/gemini-2.5-flash");
        assert_eq!(loaded.settings.jev.source, JevSource::Env);

        set_jev(
            dir.path(),
            Some(JevEntry {
                provider: id("openrouter"),
                model: ModelId::parse("some/model").unwrap(),
            }),
        )
        .unwrap();
        let mut warnings = Vec::new();
        let unkeyed = load(dir.path(), |_| None, None, |w| warnings.push(w)).unwrap();
        assert_eq!(unkeyed.jev.id(), "fake-jev", "runs still work without the model Jev");
        assert!(
            unkeyed
                .settings
                .jev
                .problem
                .as_deref()
                .unwrap()
                .contains("`openrouter` is not available")
        );
        assert_eq!(warnings.len(), 1);

        let keys = KeyStore::at(dir.path().join("keys.json"));
        keys.set(&id("openrouter"), "sk-or-saved").unwrap();
        let keyed = load(dir.path(), |_| None, Some(&keys), |_| {}).unwrap();
        assert_eq!(keyed.jev.id(), "openrouter-jev:some/model");
        assert_eq!(keyed.settings.jev.source, JevSource::File);

        set_jev(dir.path(), None).unwrap();
        assert_eq!(std::fs::read_to_string(dir.path().join(PROVIDERS_FILE)).unwrap(), "{}\n");
    }
}
