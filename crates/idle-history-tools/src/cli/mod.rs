//! Source selection and provider interpretation belong to the application.

mod error;
mod imports;
mod input;
mod output;

use clap::{Parser, Subcommand};
use error::{Failure, Result};
use idle_history_import::{reconciliation::ImportState, ImportOptions};
use output::{Format, Output};
use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, Parser)]
#[command(
    name = "idle-history-tools",
    version,
    about = "Idle source import and inspection"
)]
struct Cli {
    /// Destination chain, selected explicitly by the application or caller.
    #[arg(long, global = true, default_value = ".editchain")]
    chain: PathBuf,
    #[arg(long, global = true, value_enum, default_value = "human")]
    output: Format,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Import recorded Claude, Codex or editor source files.
    Import(Box<imports::Args>),
    /// Inspect provider derivations and logical source items.
    ImportState,
    /// Convert source observations into schema-three activities in a new chain.
    Convert {
        #[arg(long)]
        destination: PathBuf,
    },
}

pub(crate) fn run() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let code = if error.use_stderr() { 2 } else { 0 };
            if let Err(error) = error.print() {
                return report(&error.into());
            }
            return ExitCode::from(code);
        }
    };
    let options = ImportOptions::default();
    let cancellation = options.cancellation.clone();
    if let Err(error) = ctrlc::set_handler(move || cancellation.cancel()) {
        return report(&Failure::new(1, error.to_string()));
    }
    let mut output = Output::new(cli.output);
    let result = execute(cli, &options, &mut output);
    match result.and(output.finish().map_err(Into::into)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) if error.code == 0 => report(&Failure::new(
            1,
            "result pipe closed; retry the same source to recover durable work",
        )),
        Err(error) => report(&error),
    }
}

fn execute(cli: Cli, options: &ImportOptions, output: &mut Output) -> Result<()> {
    match cli.command {
        Command::Import(args) => imports::run(&cli.chain, &args, options, output),
        Command::ImportState => {
            let queries = editchain_engine::queries::ChainQueries::open(&cli.chain)?;
            output
                .emit(&ImportState::from_query(&queries)?)
                .map_err(Into::into)
        }
        Command::Convert { destination } => output
            .emit(&idle_history_import::activity::migrate(
                &cli.chain,
                &destination,
                || options.cancellation.is_cancelled(),
            )?)
            .map_err(Into::into),
    }
}

fn report(error: &Failure) -> ExitCode {
    let _written = output::diagnostic(&error.message);
    ExitCode::from(error.code)
}
