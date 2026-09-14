mod cli;
mod commands;
mod config;
mod context;
mod docker;
mod env_writer;
mod services;
mod supervisor;

use anyhow::{Context, Result};
use clap::Parser;
use cli::{Cli, Commands};
use colored::Colorize;
use context::WorkspaceContext;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{} Error: {:#}", "[error]".red().bold(), e);
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    if let Commands::Init(args) = cli.command {
        return commands::execute_init(args);
    }

    let ctx = WorkspaceContext::resolve(&cli).context("Failed to resolve workspace context")?;

    // If not local execution (e.g. running inside a cloud orchestrator sandbox), skip local Docker services
    if !ctx.is_local {
        println!(
            "{} Running in non-local environment, skipping local Docker services.",
            "[info]".blue().bold()
        );
        return Ok(());
    }

    match &cli.command {
        Commands::Init(_) => unreachable!(),
        Commands::Dev(args) => commands::execute_dev(&ctx, args),
        Commands::Teardown => commands::execute_teardown(&ctx),
        Commands::Status => commands::execute_status(&ctx),
        Commands::Env(args) => commands::execute_env(&ctx, args),
    }
}
