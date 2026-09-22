//! A DAGOS workspace: the `.dagos` directory with the project database and provider config.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dagos_core::context::FakeJev;
use dagos_core::domain::{ModelId, Project, ProviderId, RunConfig};
use dagos_core::provider::FakeProvider;
use dagos_core::runtime::Runtime;
use dagos_core::store::{EventListener, Store};

use crate::providers;

/// The project database inside the DAGOS directory.
pub const DATABASE_FILE: &str = "dagos.sqlite3";

/// The system prompt new projects start with; editable with `dagos config --system-prompt`.
pub const DEFAULT_SYSTEM_PROMPT: &str = "You are a careful coding assistant working inside DAGOS. \
Keep replies brief. Record decisions, tasks, artifacts, observations, and results worth \
remembering as emissions: prose is shown to people but not remembered.";

/// The configuration new projects start with: the offline fake provider.
pub fn default_run_config() -> RunConfig {
    RunConfig {
        provider_id: ProviderId::parse(FakeProvider::ID).expect("valid provider id"),
        model_id: ModelId::parse("fake-echo").expect("valid model id"),
        system_prompt: DEFAULT_SYSTEM_PROMPT.to_owned(),
    }
}

/// An opened workspace: its store, its project, and a runtime with the configured providers.
pub struct Workspace {
    pub store: Arc<Store>,
    pub project: Project,
    pub runtime: Runtime,
}

impl Workspace {
    /// Opens the workspace in `dir`, which `dagos init` must have created. `listener` observes
    /// every committed event.
    pub fn open(
        dir: &Path,
        listener: Option<EventListener>,
        inference_timeout: Duration,
    ) -> Result<Self, String> {
        let database = dir.join(DATABASE_FILE);
        if !database.exists() {
            return Err(format!("no DAGOS workspace in {}; run `dagos init` first", dir.display()));
        }
        let mut store = open_store(&database)?;
        if let Some(listener) = listener {
            store = store.with_event_listener(listener);
        }
        let store = Arc::new(store);
        let project = store
            .transaction(|tx| tx.projects())
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or_else(|| format!("the database in {} has no project", dir.display()))?;
        let providers = providers::load(
            dir,
            |name| std::env::var(name).ok(),
            |warning| eprintln!("warning: {warning}"),
        )?;
        let mut runtime = Runtime::new(store.clone(), Arc::new(FakeJev::new()))
            .with_inference_timeout(inference_timeout);
        for provider in providers {
            runtime = runtime.with_provider(provider);
        }
        Ok(Self { store, project, runtime })
    }

    /// The configuration new runs use: the project's defaults, or the built-in default.
    pub fn run_config(&self) -> Result<RunConfig, String> {
        Ok(self
            .runtime
            .defaults(&self.project.id)
            .map_err(|error| error.to_string())?
            .unwrap_or_else(default_run_config))
    }
}

fn open_store(database: &Path) -> Result<Store, String> {
    Store::open(database).map_err(|error| format!("cannot open {}: {error}", database.display()))
}

/// Creates the DAGOS directory, database, project, and default run configuration unless they
/// exist. Returns the project and whether it was created now.
pub fn init(dir: &Path, name: Option<&str>) -> Result<(Project, bool), String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let store = open_store(&dir.join(DATABASE_FILE))?;
    let existing = store.transaction(|tx| tx.projects()).map_err(|error| error.to_string())?;
    if let Some(project) = existing.into_iter().next() {
        return Ok((project, false));
    }
    let name = name.map(str::to_owned).unwrap_or_else(|| default_project_name(dir));
    let project = store
        .transaction(|tx| {
            let project = tx.create_project(&name)?;
            tx.set_run_defaults(&project.id, &default_run_config())?;
            Ok::<_, dagos_core::store::StoreError>(project)
        })
        .map_err(|error| error.to_string())?;
    Ok((project, true))
}

/// The name of the directory that contains the DAGOS directory, e.g. `my-app` for `my-app/.dagos`.
fn default_project_name(dir: &Path) -> String {
    let absolute = std::path::absolute(dir).unwrap_or_else(|_| PathBuf::from(dir));
    absolute
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "dagos".to_owned())
}
