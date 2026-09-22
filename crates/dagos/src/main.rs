//! `dagos` command-line interface: the transport boundary around `dagos-core`.

use clap::Parser;

/// DAGOS: a minimal DAG operating system for LLM coding workflows.
#[derive(Debug, Parser)]
#[command(name = "dagos", version, about)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
