use crate::context::WorkspaceContext;
use crate::docker::{DockerCompose, reclaim_stale_ports};
use crate::env_writer::EnvWriter;
use anyhow::Result;
use colored::Colorize;

pub fn execute_bootstrap(ctx: &WorkspaceContext) -> Result<()> {
    println!(
        "{} Bootstrapping environment for project '{}' (project: {})...",
        "[bootstrap]".blue().bold(),
        ctx.config.name,
        ctx.compose_project.cyan()
    );

    // 1. Free ports from other ai-igniter projects
    reclaim_stale_ports(ctx);

    // 2. Generate compose from current config
    let compose = DockerCompose::new(ctx)?;

    // 3. Seed workspace files and write the managed environment file
    EnvWriter::write_workspace_env(ctx)?;

    // 4. Create containers and named volumes without starting services
    compose.create()?;

    println!(
        "{} Environment bootstrap complete! Containers and volumes created, .env written.",
        "[bootstrap]".green().bold()
    );

    Ok(())
}
