use crate::cli::DevArgs;
use crate::context::WorkspaceContext;
use crate::docker::{DockerCompose, reclaim_stale_ports};
use crate::env_writer::EnvWriter;
use crate::services::get_active_services;
use crate::supervisor::{ShutdownSignal, Supervisor};
use anyhow::{Context, Result};
use colored::Colorize;

pub fn execute_dev(ctx: &WorkspaceContext, args: &DevArgs) -> Result<()> {
    let shutdown = ShutdownSignal::install()?;

    println!(
        "{} Starting dev environment for project '{}' (project: {})...",
        "[dev]".blue().bold(),
        ctx.config.name,
        ctx.compose_project.cyan()
    );

    // 1. Free our ports from other ai-igniter projects
    reclaim_stale_ports(ctx);

    // 2. Regenerate compose from the current config
    let compose = DockerCompose::new(ctx)?;
    if args.reset {
        println!(
            "{} Resetting services for project '{}' (wiping volumes)...",
            "[dev]".yellow().bold(),
            ctx.compose_project.cyan()
        );
        compose.down(true)?;
    }

    // 3. Write workspace .env
    EnvWriter::write_workspace_env(ctx)?;

    // 4. Start, initialize and supervise; services are stopped whatever the outcome
    let result = start_and_supervise(ctx, &compose, &shutdown);
    if let Err(e) = compose.stop() {
        eprintln!("{} Warning: {:#}", "[dev]".yellow().bold(), e);
    }

    // An interrupt during startup aborts child processes: that is a clean shutdown, not a failure
    if shutdown.requested() { Ok(()) } else { result }
}

fn start_and_supervise(ctx: &WorkspaceContext, compose: &DockerCompose<'_>, shutdown: &ShutdownSignal) -> Result<()> {
    compose.up()?;

    for service in get_active_services(ctx) {
        if shutdown.requested() {
            return Ok(());
        }
        service
            .post_start(ctx, compose)
            .with_context(|| format!("Service '{}' failed to initialize", service.name()))?;
    }

    if shutdown.requested() {
        return Ok(());
    }
    Supervisor::run_dev(ctx, compose, shutdown)
}
