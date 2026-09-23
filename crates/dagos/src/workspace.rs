//! A DAGOS workspace: the `.dagos` directory with the project database and provider config.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use dagos_core::domain::{ModelId, Project, ProviderId, RunConfig};
use dagos_core::provider::FakeProvider;
use dagos_core::runtime::Runtime;
use dagos_core::store::{EventListener, Store, StoreError};
use dagos_mcp::{Capabilities, McpConfig};

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

/// The optional MCP configuration inside the DAGOS directory.
pub const MCP_FILE: &str = "mcp.json";

/// How long each MCP server may take to describe its tools.
const MCP_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// An opened workspace: its store, its project, and a runtime with the configured providers.
pub struct Workspace {
    pub store: Arc<Store>,
    pub project: Project,
    pub runtime: Runtime,
    /// MCP capabilities discovered by this process, if discovery ran (`run` and `serve` do).
    pub capabilities: Option<Capabilities>,
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
        let loaded = providers::load(
            dir,
            |name| std::env::var(name).ok(),
            |warning| eprintln!("warning: {warning}"),
        )?;
        let mut runtime =
            Runtime::new(store.clone(), loaded.jev).with_inference_timeout(inference_timeout);
        for provider in loaded.providers {
            runtime = runtime.with_provider(provider);
        }
        Ok(Self { store, project, runtime, capabilities: None })
    }

    /// Discovers MCP capabilities from `mcp.json` in `dir` and compiles them into every
    /// subsequent run's IR. Without the file, or with every server unavailable, runs simply have
    /// no tools; `warn` hears about each server that could not be used.
    pub async fn with_mcp_capabilities(
        mut self,
        dir: &Path,
        mut warn: impl FnMut(String),
    ) -> Result<Self, String> {
        let Some(config) = McpConfig::load(&dir.join(MCP_FILE))? else { return Ok(self) };
        let capabilities = dagos_mcp::discover(&config, MCP_DISCOVERY_TIMEOUT).await;
        for server in &capabilities.servers {
            if let Some(error) = &server.error {
                warn(format!(
                    "MCP server `{}` unavailable, continuing without it: {error}",
                    server.id
                ));
            }
        }
        self.runtime = self.runtime.with_tools(capabilities.tools.clone());
        self.capabilities = Some(capabilities);
        Ok(self)
    }

    /// The configuration new runs use: the project's defaults, or the built-in default.
    pub fn run_config(&self) -> Result<RunConfig, StoreError> {
        Ok(self.runtime.defaults(&self.project.id)?.unwrap_or_else(default_run_config))
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
