pub mod garage;
pub mod postgres;

use crate::context::WorkspaceContext;
use crate::docker::DockerCompose;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::process::Command;

pub trait Service {
    fn name(&self) -> &str;
    /// Runs once containers are healthy (`docker compose up --wait`): buckets, databases, migrations, seeds.
    fn post_start(&self, ctx: &WorkspaceContext, compose: &DockerCompose<'_>) -> Result<()>;
}

/// Services with post-start work, in execution order (storage first, so seeds can upload files).
pub fn get_active_services(ctx: &WorkspaceContext) -> Vec<Box<dyn Service + '_>> {
    let services = &ctx.config.services;
    let mut active: Vec<Box<dyn Service + '_>> = Vec::new();

    if let Some(garage) = services.garage() {
        active.push(Box::new(garage::GarageService { config: garage }));
    }
    if let Some(pg) = services.postgres() {
        active.push(Box::new(postgres::PostgresService { config: pg }));
    }

    active
}

pub fn run_shell(ctx: &WorkspaceContext, command: &str, env: &BTreeMap<String, String>) -> Result<()> {
    let status = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(&ctx.workspace_path)
        .envs(env)
        .status()
        .with_context(|| format!("Failed to run `{command}`"))?;
    if !status.success() {
        bail!("`{command}` exited with {status}");
    }
    Ok(())
}
