//! A DAGOS workspace: the `.dagos` directory with the project database and provider config.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::Duration;

use dagos_core::domain::{IrTool, ModelId, Project, ProjectId, ProviderId, RunConfig};
use dagos_core::provider::FakeProvider;
use dagos_core::runtime::Runtime;
use dagos_core::store::{EventListener, Store, StoreError};
use dagos_core::tools::{ToolExecutor, ToolGate};
use dagos_mcp::{Capabilities, McpConfig, McpPool};

use crate::approvals::{APPROVAL_TIMEOUT, Approvals};

use crate::keys::KeyStore;
use crate::providers::{self, Endpoint, Settings};

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

/// How long each MCP server may take to start and describe its tools (a cold start of a Node
/// server can take several seconds).
const MCP_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// An opened workspace: its store, its project, and a runtime with the configured providers.
///
/// Provider configuration can change while the workspace is open (keys saved in the app, custom
/// endpoints, the Jev choice); [`Workspace::reload`] then swaps in a new runtime. Runs already
/// executing keep the runtime they started with.
pub struct Workspace {
    pub store: Arc<Store>,
    pub project: Project,
    dir: PathBuf,
    keys: Option<KeyStore>,
    inference_timeout: Duration,
    /// MCP tools, once [`Workspace::start_tools`] has run (`run` and `serve` do).
    mcp: RwLock<Option<McpState>>,
    approvals: Arc<Approvals>,
    live: RwLock<Live>,
    edits: Mutex<()>,
}

/// The running MCP servers and what they offer.
struct McpState {
    pool: Arc<McpPool>,
    capabilities: Capabilities,
}

/// What the current provider configuration produced.
struct Live {
    runtime: Arc<Runtime>,
    settings: Settings,
    endpoints: BTreeMap<ProviderId, Endpoint>,
}

impl Workspace {
    /// Opens the workspace in `dir`, which `dagos init` must have created, with the per-user
    /// saved keys. `listener` observes every committed event.
    pub fn open(
        dir: &Path,
        listener: Option<EventListener>,
        inference_timeout: Duration,
    ) -> Result<Self, String> {
        let keys = KeyStore::for_user(|name| std::env::var(name).ok());
        Self::open_with_keys(dir, listener, inference_timeout, keys)
    }

    /// [`Workspace::open`] with saved keys from `keys` (or none).
    pub fn open_with_keys(
        dir: &Path,
        listener: Option<EventListener>,
        inference_timeout: Duration,
        keys: Option<KeyStore>,
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
        let live = build(&store, dir, keys.as_ref(), inference_timeout, None)?;
        Ok(Self {
            store,
            project,
            dir: dir.to_owned(),
            keys,
            inference_timeout,
            mcp: RwLock::new(None),
            approvals: Arc::new(Approvals::new(APPROVAL_TIMEOUT)),
            live: RwLock::new(live),
            edits: Mutex::new(()),
        })
    }

    /// The runtime new runs use.
    pub fn runtime(&self) -> Arc<Runtime> {
        self.live.read().unwrap_or_else(PoisonError::into_inner).runtime.clone()
    }

    /// The provider and Jev settings, without keys.
    pub fn settings(&self) -> Settings {
        self.live.read().unwrap_or_else(PoisonError::into_inner).settings.clone()
    }

    /// The configured OpenAI-compatible endpoint `id`, for connection checks.
    pub fn endpoint(&self, id: &ProviderId) -> Option<Endpoint> {
        self.live.read().unwrap_or_else(PoisonError::into_inner).endpoints.get(id).cloned()
    }

    /// The DAGOS directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where keys saved in the app live, if anywhere.
    pub fn keys(&self) -> Option<&KeyStore> {
        self.keys.as_ref()
    }

    /// Serializes configuration edits; hold it across an edit and the [`Workspace::reload`].
    pub fn edit_lock(&self) -> MutexGuard<'_, ()> {
        self.edits.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Rebuilds the providers, the Jev, and the runtime from the current configuration. On error
    /// the previous runtime stays in place.
    pub fn reload(&self) -> Result<(), String> {
        let tools = {
            let mcp = self.mcp.read().unwrap_or_else(PoisonError::into_inner);
            mcp.as_ref().map(|state| {
                let runner: ToolRunner = (state.pool.clone(), self.approvals.clone());
                (state.capabilities.tools.clone(), runner)
            })
        };
        let live =
            build(&self.store, &self.dir, self.keys.as_ref(), self.inference_timeout, tools)?;
        *self.live.write().unwrap_or_else(PoisonError::into_inner) = live;
        Ok(())
    }

    /// Starts the MCP servers named in `mcp.json` (stopping any this workspace ran before) and
    /// offers their tools to subsequent runs. Without the file runs simply have no tools; `warn`
    /// hears about each server that could not be used.
    pub async fn start_tools(&self, mut warn: impl FnMut(String)) -> Result<(), String> {
        let state = match McpConfig::load(&self.dir.join(MCP_FILE))? {
            None => None,
            Some(config) => {
                let pool = Arc::new(McpPool::new(config.clone(), MCP_DISCOVERY_TIMEOUT));
                let capabilities = pool.discover().await;
                for server in &capabilities.servers {
                    if let Some(error) = &server.error {
                        warn(format!(
                            "MCP server `{}` unavailable, continuing without it: {error}",
                            server.id
                        ));
                    }
                }
                self.approvals.set_config(config);
                Some(McpState { pool, capabilities })
            }
        };
        *self.mcp.write().unwrap_or_else(PoisonError::into_inner) = state;
        self.reload()
    }

    /// Saves new tool policies to `mcp.json` and applies them without restarting servers. The
    /// servers themselves (ids, commands) must be unchanged; use [`Workspace::start_tools`] after
    /// changing those.
    pub fn set_tool_policies(&self, config: McpConfig) -> Result<(), String> {
        config.save(&self.dir.join(MCP_FILE))?;
        {
            let mut mcp = self.mcp.write().unwrap_or_else(PoisonError::into_inner);
            if let Some(state) = mcp.as_mut() {
                state.capabilities = state.pool.set_policies(config.clone());
            }
        }
        self.approvals.set_config(config);
        self.reload()
    }

    /// The MCP configuration in `mcp.json` (empty if there is none).
    pub fn mcp_config(&self) -> Result<McpConfig, String> {
        Ok(McpConfig::load(&self.dir.join(MCP_FILE))?.unwrap_or_default())
    }

    /// What the MCP servers offer, if tools were started.
    pub fn capabilities(&self) -> Option<Capabilities> {
        let mcp = self.mcp.read().unwrap_or_else(PoisonError::into_inner);
        mcp.as_ref().map(|state| state.capabilities.clone())
    }

    /// The gate that decides and collects approvals for tool calls.
    pub fn approvals(&self) -> &Arc<Approvals> {
        &self.approvals
    }

    /// The configuration new runs of the default project use.
    pub fn run_config(&self) -> Result<RunConfig, StoreError> {
        self.run_config_for(&self.project.id)
    }

    /// The configuration new runs of `project_id` use: its defaults, or the built-in default.
    pub fn run_config_for(&self, project_id: &ProjectId) -> Result<RunConfig, StoreError> {
        Ok(self.runtime().defaults(project_id)?.unwrap_or_else(default_run_config))
    }

    /// Creates a project whose runs start with `defaults`.
    pub fn create_project(&self, name: &str, defaults: &RunConfig) -> Result<Project, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("the project name is empty".into());
        }
        self.store
            .transaction(|tx| {
                let project = tx.create_project(name)?;
                tx.set_run_defaults(&project.id, defaults)?;
                Ok::<_, StoreError>(project)
            })
            .map_err(|error| error.to_string())
    }
}

/// What executes tool calls and what decides whether they may run.
type ToolRunner = (Arc<dyn ToolExecutor>, Arc<dyn ToolGate>);

/// A runtime over `store` with the providers and Jev configured for `dir`, and the MCP tools
/// with their runner, if any.
fn build(
    store: &Arc<Store>,
    dir: &Path,
    keys: Option<&KeyStore>,
    inference_timeout: Duration,
    tools: Option<(Vec<IrTool>, ToolRunner)>,
) -> Result<Live, String> {
    let loaded = providers::load(
        dir,
        |name| std::env::var(name).ok(),
        keys,
        |warning| eprintln!("warning: {warning}"),
    )?;
    let mut runtime =
        Runtime::new(store.clone(), loaded.jev.clone()).with_inference_timeout(inference_timeout);
    if let Some((tools, (executor, gate))) = tools {
        runtime = runtime.with_tools(tools).with_tool_runner(executor, gate);
    }
    if !Arc::ptr_eq(&loaded.jev, &loaded.jev_fallback) {
        runtime = runtime.with_jev_fallback(loaded.jev_fallback);
    }
    for provider in loaded.providers {
        runtime = runtime.with_provider(provider);
    }
    Ok(Live { runtime: Arc::new(runtime), settings: loaded.settings, endpoints: loaded.endpoints })
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
