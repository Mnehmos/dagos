//! `dagos` command-line interface: the transport boundary around `dagos-core`.

mod providers;
mod workspace;

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use dagos_core::domain::{EventData, ModelId, ProviderId, RunConfig, RunStatus};
use dagos_core::store::EventListener;
use serde_json::json;

use workspace::Workspace;

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
                let config = args.selection.apply(workspace.run_config()?)?;
                workspace
                    .runtime
                    .set_defaults(&workspace.project.id, &config)
                    .map_err(|e| e.to_string())?;
            }
            let providers: Vec<_> = workspace
                .runtime
                .providers()
                .map(|provider| json!({"id": provider.id(), "suggested_models": provider.suggested_models()}))
                .collect();
            let report = json!({
                "project": workspace.project,
                "run_defaults": workspace.run_config()?,
                "providers": providers,
            });
            println!("{}", serde_json::to_string_pretty(&report).expect("report serializes"));
            Ok(ExitCode::SUCCESS)
        }
        Command::Run(args) => run(&cli.dir, args, timeout).await,
        Command::Recover => {
            let workspace = Workspace::open(&cli.dir, None, timeout)?;
            let recovered =
                workspace.runtime.recover_interrupted_runs().map_err(|e| e.to_string())?;
            println!("Marked {} interrupted run(s) as failed.", recovered.len());
            Ok(ExitCode::SUCCESS)
        }
    }
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
                    _ => {}
                }
            }
            let _ = stdout.flush();
        }))
    };
    let workspace = Workspace::open(dir, listener, timeout)?;
    let config = args.selection.apply(workspace.run_config()?)?;
    let run = workspace
        .runtime
        .run(&workspace.project.id, &args.message, &config)
        .await
        .map_err(|error| error.to_string())?;

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
