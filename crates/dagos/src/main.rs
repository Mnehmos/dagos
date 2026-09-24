//! `dagos` command-line interface: the transport boundary around `dagos-core`.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use dagos_core::domain::{EventData, ModelId, ProviderId, RunConfig, RunStatus};
use dagos_core::provider::FakeProvider;
use dagos_core::runtime::Thread;
use dagos_core::store::EventListener;
use serde_json::json;

use dagos::inspect;
use dagos::keys::{self, KeyStore};
use dagos::server;
use dagos::workspace::{self, Workspace};

/// DAGOS: a minimal DAG operating system for LLM coding workflows.
#[derive(Debug, Parser)]
#[command(name = "dagos", version, about)]
struct Cli {
    /// The DAGOS directory holding the project database and provider configuration.
    #[arg(long, global = true, env = "DAGOS_DIR", default_value = ".dagos")]
    dir: PathBuf,
    /// Seconds a provider may take to return its final output before the run fails.
    #[arg(long, global = true, env = "DAGOS_INFERENCE_TIMEOUT", default_value_t = 180)]
    inference_timeout: u64,
    #[command(subcommand)]
    command: Command,
}

impl Cli {
    fn timeout(&self) -> Duration {
        Duration::from_secs(self.inference_timeout)
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create the DAGOS directory, database, and project (does nothing if they exist).
    Init {
        /// Project name (default: the name of the directory containing the DAGOS directory).
        #[arg(long)]
        name: Option<String>,
    },
    /// Show the providers and the configuration new runs use, or change it.
    Config(ConfigArgs),
    /// Run the pipeline for one message and stream the reply.
    Run(RunArgs),
    /// Mark runs left running by a crashed process as failed (`interrupted`).
    Recover,
    /// Print recorded state as JSON (read-only).
    Inspect {
        #[command(subcommand)]
        what: Option<Inspect>,
    },
    /// Manage API keys saved for this user (an environment variable always wins).
    Keys {
        #[command(subcommand)]
        action: Option<Keys>,
    },
    /// Lint functions against the project's plain-English rules in `.dagos/lint.json` (needs a
    /// Jev that answers yes/no questions, e.g. TypeSafe's). Exits 1 when a rule applies.
    Lint {
        /// Files to lint, relative to the project root (default: files changed since the last
        /// commit, including new ones).
        files: Vec<String>,
        /// The probability at which a rule counts as applying (default: the configured one).
        #[arg(long)]
        threshold: Option<f64>,
        /// Print every judgment as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Serve the inspection API on a local port.
    Serve {
        /// Address to bind: a loopback address, since the app runs tools on this computer and has
        /// no sign-in.
        #[arg(long, default_value = "127.0.0.1:7420")]
        address: String,
    },
}

/// Saved API key actions.
#[derive(Debug, Subcommand)]
enum Keys {
    /// Where keys are saved and which providers have one (the default). Never prints keys.
    List,
    /// Save a key for a provider, read from standard input so it stays out of shell history.
    Set { provider: String },
    /// Remove the saved key for a provider.
    Remove { provider: String },
}

/// What to inspect. Runs are named by ID or `latest`.
#[derive(Debug, Subcommand)]
enum Inspect {
    /// The project: run defaults, providers, the durable DAG, and runs (the default).
    Overview,
    /// The durable DAG: nodes and edges.
    Dag,
    /// Everything recorded about a run.
    Run { run: Option<String> },
    /// A run's ordered event history.
    Events { run: Option<String> },
    /// A run's active context.
    Context { run: Option<String> },
    /// The IR a run's provider received.
    Ir { run: Option<String> },
    /// A run's validated structured response.
    Response { run: Option<String> },
}

/// Overrides for provider, model, and system prompt.
#[derive(Debug, Args)]
struct Selection {
    /// Inference provider, e.g. `fake` or `openrouter`.
    #[arg(long)]
    provider: Option<String>,
    /// Model ID, passed to the provider unchanged, e.g. `fake-echo`.
    #[arg(long)]
    model: Option<String>,
    /// System prompt text.
    #[arg(long, conflicts_with = "system_prompt_file")]
    system_prompt: Option<String>,
    /// Read the system prompt from a file.
    #[arg(long)]
    system_prompt_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ConfigArgs {
    #[command(flatten)]
    selection: Selection,
}

#[derive(Debug, Args)]
struct RunArgs {
    /// The message to send.
    message: String,
    #[command(flatten)]
    selection: Selection,
    /// Print the finished run as JSON instead of streaming the reply.
    #[arg(long)]
    json: bool,
    /// Start a new conversation instead of continuing the latest one.
    #[arg(long)]
    new: bool,
}

impl Selection {
    fn is_empty(&self) -> bool {
        self.provider.is_none()
            && self.model.is_none()
            && self.system_prompt.is_none()
            && self.system_prompt_file.is_none()
    }

    /// `base` with these overrides applied.
    fn apply(&self, mut base: RunConfig) -> Result<RunConfig, String> {
        if let Some(provider) = &self.provider {
            base.provider_id = ProviderId::parse(provider.as_str()).map_err(|e| e.to_string())?;
        }
        if let Some(model) = &self.model {
            base.model_id = ModelId::parse(model.as_str()).map_err(|e| e.to_string())?;
        }
        if let Some(prompt) = &self.system_prompt {
            base.system_prompt = prompt.clone();
        }
        if let Some(path) = &self.system_prompt_file {
            base.system_prompt = std::fs::read_to_string(path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        }
        Ok(base)
    }
}

async fn lint(
    dir: &std::path::Path,
    files: Vec<String>,
    threshold: Option<f64>,
    as_json: bool,
    timeout: Duration,
) -> Result<ExitCode, String> {
    let workspace = Workspace::open(dir, None, timeout)?;
    let root = workspace::project_root(dir);
    let mut config = dagos_lint::LintConfig::load(&dir.join(dagos_lint::LINT_FILE))?;
    if let Some(threshold) = threshold {
        config.threshold = threshold;
    }
    let files = if files.is_empty() { changed_files(&root)? } else { files };
    let jev = workspace.runtime().shared_jev();
    let report = dagos_lint::lint_files(&root, &files, jev, &config).await?;
    if as_json {
        let judgments: Vec<_> = report
            .units
            .iter()
            .zip(&report.judgments)
            .map(|(unit, row)| {
                let scores: serde_json::Map<_, _> =
                    config.rules.iter().zip(row).map(|(rule, p)| (rule.id.clone(), json!(p))).collect();
                json!({"file": unit.file, "function": unit.function.name, "line": unit.function.line, "judgments": scores})
            })
            .collect();
        let out = json!({"threshold": config.threshold, "functions": judgments, "findings": report.findings});
        println!("{}", serde_json::to_string_pretty(&out).expect("report serializes"));
    } else {
        for finding in &report.findings {
            println!(
                "{}:{} {}  {} ({:.2})",
                finding.file, finding.line, finding.function, finding.text, finding.probability
            );
        }
        println!(
            "{} function(s), {} rule(s), {} judgment(s): {} finding(s) at {:.2} or above.",
            report.units.len(),
            config.rules.len(),
            report.units.len() * config.rules.len(),
            report.findings.len(),
            config.threshold
        );
    }
    Ok(if report.findings.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(1) })
}

/// Files changed since the last commit, and new files, relative to `root` (from git).
fn changed_files(root: &std::path::Path) -> Result<Vec<String>, String> {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=all"])
        .current_dir(root)
        .output()
        .map_err(|error| format!("cannot run git to find changed files: {error}"))?;
    if !output.status.success() {
        return Err("git status failed; name the files to lint".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.len() > 3 && !line.starts_with(" D") && !line.starts_with("D "))
        .map(|line| {
            let path = &line[3..];
            path.rsplit(" -> ").next().unwrap_or(path).trim_matches('"').to_owned()
        })
        .collect())
}

fn keys(action: Keys) -> Result<ExitCode, String> {
    let store = KeyStore::for_user(|name| std::env::var(name).ok())
        .ok_or("no user configuration directory; set DAGOS_CONFIG_DIR")?;
    let provider = |id: &str| {
        let id = ProviderId::parse(id).map_err(|error| error.to_string())?;
        if id.as_str() == FakeProvider::ID {
            return Err("the fake provider needs no key".to_owned());
        }
        Ok(id)
    };
    let report = match action {
        Keys::List => {
            let saved: serde_json::Map<_, _> = store
                .load()?
                .iter()
                .map(|(id, key)| (id.to_string(), json!({"hint": keys::hint(key)})))
                .collect();
            json!({"keys_file": store.path(), "saved": saved})
        }
        Keys::Set { provider: id } => {
            let id = provider(&id)?;
            let mut key = String::new();
            std::io::stdin()
                .read_line(&mut key)
                .map_err(|error| format!("cannot read the key from standard input: {error}"))?;
            store.set(&id, &key)?;
            json!({"saved": id, "hint": keys::hint(keys::validate(&key)?)})
        }
        Keys::Remove { provider: id } => {
            let id = provider(&id)?;
            json!({"removed": store.remove(&id)?, "provider": id})
        }
    };
    println!("{}", serde_json::to_string_pretty(&report).expect("report serializes"));
    Ok(ExitCode::SUCCESS)
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match execute(cli).await {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::from(2)
        }
    }
}

async fn execute(cli: Cli) -> Result<ExitCode, String> {
    let timeout = cli.timeout();
    match cli.command {
        Command::Init { name } => {
            let (project, created) = workspace::init(&cli.dir, name.as_deref())?;
            let verb = if created { "Initialized" } else { "Already initialized:" };
            println!(
                "{verb} DAGOS project `{}` ({}) in {}",
                project.name,
                project.id,
                cli.dir.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Config(args) => {
            let workspace = Workspace::open(&cli.dir, None, timeout)?;
            if !args.selection.is_empty() {
                let config =
                    args.selection.apply(workspace.run_config().map_err(|e| e.to_string())?)?;
                workspace
                    .runtime()
                    .set_defaults(&workspace.project.id, &config)
                    .map_err(|e| e.to_string())?;
            }
            let providers: Vec<_> = workspace
                .runtime()
                .providers()
                .map(|provider| json!({"id": provider.id(), "suggested_models": provider.suggested_models()}))
                .collect();
            let report = json!({
                "project": workspace.project,
                "run_defaults": workspace.run_config().map_err(|e| e.to_string())?,
                "providers": providers,
            });
            println!("{}", serde_json::to_string_pretty(&report).expect("report serializes"));
            Ok(ExitCode::SUCCESS)
        }
        Command::Run(args) => run(&cli.dir, args, timeout).await,
        Command::Lint { files, threshold, json } => {
            lint(&cli.dir, files, threshold, json, timeout).await
        }
        Command::Recover => {
            let workspace = Workspace::open(&cli.dir, None, timeout)?;
            let recovered =
                workspace.runtime().recover_interrupted_runs().map_err(|e| e.to_string())?;
            println!("Marked {} interrupted run(s) as failed.", recovered.len());
            Ok(ExitCode::SUCCESS)
        }
        Command::Inspect { what } => {
            let workspace = Workspace::open(&cli.dir, None, timeout)?;
            let document = inspect_view(&workspace, what.unwrap_or(Inspect::Overview))?;
            println!("{}", serde_json::to_string_pretty(&document).expect("views serialize"));
            Ok(ExitCode::SUCCESS)
        }
        Command::Keys { action } => keys(action.unwrap_or(Keys::List)),
        Command::Serve { address } => {
            let hub = server::EventHub::new();
            let workspace = Workspace::open(&cli.dir, Some(hub.listener()), timeout)?;
            // The app can ask a person to approve tool calls.
            workspace.approvals().set_interactive(true);
            let workspace = Arc::new(workspace);
            // MCP servers start in the background: the app is usable at once, and runs get the
            // tools as soon as they are ready.
            let tools = workspace.clone();
            tokio::spawn(async move {
                if let Err(error) =
                    tools.start_tools(|warning| eprintln!("warning: {warning}")).await
                {
                    eprintln!("warning: MCP tools are unavailable: {error}");
                }
            });
            let listener = tokio::net::TcpListener::bind(&address)
                .await
                .map_err(|error| format!("cannot listen on {address}: {error}"))?;
            let bound = listener.local_addr().map_err(|e| e.to_string())?;
            let name = &workspace.project.name;
            eprintln!("DAGOS inspector for `{name}` on http://{bound}/");
            server::serve(listener, workspace, hub).await.map_err(|e| e.to_string())?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn to_json(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("views serialize")
}

/// The JSON document for one `dagos inspect` request.
fn inspect_view(workspace: &Workspace, what: Inspect) -> Result<serde_json::Value, String> {
    let detail = |reference: Option<String>| -> Result<inspect::RunDetail, String> {
        let reference = reference.unwrap_or_else(|| "latest".to_owned());
        let not_found = || format!("run `{reference}` not found");
        let id = inspect::resolve_run(&workspace.store, &workspace.project.id, &reference)
            .map_err(|e| e.to_string())?
            .ok_or_else(not_found)?;
        inspect::run_detail(&workspace.store, &id).map_err(|e| e.to_string())?.ok_or_else(not_found)
    };
    let overview = || inspect::overview(workspace).map_err(|e| e.to_string());
    Ok(match what {
        Inspect::Overview => to_json(&overview()?),
        Inspect::Dag => to_json(&overview()?.dag),
        Inspect::Run { run } => to_json(&detail(run)?),
        Inspect::Events { run } => to_json(&detail(run)?.events),
        Inspect::Context { run } => to_json(&detail(run)?.context),
        Inspect::Ir { run } => to_json(&detail(run)?.ir),
        Inspect::Response { run } => to_json(&detail(run)?.response),
    })
}

async fn run(dir: &std::path::Path, args: RunArgs, timeout: Duration) -> Result<ExitCode, String> {
    // Stream presentation prose to stdout as it is committed; everything else goes to stderr.
    let listener: Option<EventListener> = if args.json {
        None
    } else {
        Some(Box::new(|events| {
            let mut stdout = std::io::stdout().lock();
            for event in events {
                match &event.data {
                    EventData::RunStarted { provider_id, model_id, .. } => {
                        eprintln!("run {} · {provider_id} / {model_id}", event.run_id);
                    }
                    EventData::InferenceDelta { text } => {
                        let _ = write!(stdout, "{text}");
                    }
                    EventData::JevFallback { jev_id, reason } => {
                        eprintln!("note: Jev fell back to {jev_id}: {reason}");
                    }
                    _ => {}
                }
            }
            let _ = stdout.flush();
        }))
    };
    let workspace = Workspace::open(dir, listener, timeout)?;
    workspace.start_tools(|warning| eprintln!("warning: {warning}")).await?;
    let config = args.selection.apply(workspace.run_config().map_err(|e| e.to_string())?)?;
    let project = &workspace.project.id;
    let thread = if args.new { Thread::New(project) } else { Thread::Latest(project) };
    let runtime = workspace.runtime();
    let started =
        runtime.start_in(thread, &args.message, &config).map_err(|error| error.to_string())?;
    let run = runtime.finish(started).await.map_err(|error| error.to_string())?;

    let events = workspace.store.transaction(|tx| tx.events(&run.id)).map_err(|e| e.to_string())?;
    let (mut nodes, mut edges, mut prose, mut failure) = (0, 0, None, None);
    for event in &events {
        match &event.data {
            EventData::DagNodeCreated { .. } => nodes += 1,
            EventData::DagEdgeCreated { .. } => edges += 1,
            EventData::ResponseValidated { response } => {
                prose = Some(response.presentation.prose.clone())
            }
            EventData::RunFailed { message, .. } => failure = Some(message.clone()),
            _ => {}
        }
    }
    if args.json {
        let report =
            json!({"run": run, "prose": prose, "emitted": {"nodes": nodes, "edges": edges}});
        println!("{}", serde_json::to_string_pretty(&report).expect("report serializes"));
    } else {
        println!();
        match (&run.status, &run.error_code) {
            (RunStatus::Completed, _) => {
                eprintln!("completed · {nodes} node(s), {edges} edge(s) recorded")
            }
            (_, Some(code)) => eprintln!("failed [{code}]: {}", failure.unwrap_or_default()),
            _ => eprintln!("{}", run.status),
        }
    }
    Ok(if run.status == RunStatus::Completed { ExitCode::SUCCESS } else { ExitCode::from(1) })
}
