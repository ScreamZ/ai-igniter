pub mod garage;
pub mod postgres;

use crate::config::ServicesConfig;
use crate::context::WorkspaceContext;
use crate::docker::DockerCompose;
use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// Lifecycle trait for services with post-start work (migrations, seeds, bucket setup, etc.)
pub trait Service {
    fn name(&self) -> &str;
    /// Runs once containers are healthy (`docker compose up --wait`): buckets, databases, migrations, seeds.
    fn post_start(&self, ctx: &WorkspaceContext, compose: &DockerCompose<'_>) -> Result<()>;
}

/// Service provider trait defining everything needed to support a service in ai-igniter:
/// - Configuration and port allocations
/// - Init wizard prompts and template variables
/// - Docker compose definitions and auxiliary file generation
/// - Post-start lifecycle hooks
pub trait ServiceProvider: Sync {
    /// Internal service name (e.g. "postgres", "garage")
    fn name(&self) -> &'static str;

    /// Label displayed in the multi-select during `ai-igniter init`
    fn init_label(&self) -> &'static str;

    /// Default selection status in `ai-igniter init`
    fn init_default_selected(&self) -> bool {
        true
    }

    /// Reserved port allocation names that custom services cannot use
    fn reserved_names(&self) -> Vec<&'static str> {
        vec![self.name()]
    }

    /// Collects host port offsets if this service is enabled in the configuration
    fn port_offsets(&self, services: &ServicesConfig) -> Vec<(String, u16)>;

    /// Interactive or non-interactive prompt for `ai-igniter init`.
    /// Returns any env template entries that should be added to `[env_template]`.
    fn prompt_init(
        &self,
        services: &mut ServicesConfig,
        project_name: &str,
        non_interactive: bool,
    ) -> Result<BTreeMap<String, String>>;

    /// Writes any auxiliary configuration files needed by the container before starting Compose
    fn write_auxiliary_files(&self, _ctx: &WorkspaceContext, _dir: &Path) -> Result<()> {
        Ok(())
    }

    /// Appends container and volume definitions into the Docker Compose document
    fn contribute_compose(
        &self,
        ctx: &WorkspaceContext,
        services: &mut Map<String, Value>,
        volumes: &mut Map<String, Value>,
    );

    /// Appends template variables exposed by this service for `.env` interpolation
    fn contribute_template_vars(&self, ctx: &WorkspaceContext, vars: &mut BTreeMap<String, String>);

    /// Returns a post-start service handler if this service has post-start work and is enabled
    fn get_active_service<'a>(&self, ctx: &'a WorkspaceContext) -> Option<Box<dyn Service + 'a>>;
}

pub static BUILTIN_SERVICES: &[&'static dyn ServiceProvider] =
    &[&garage::GarageProvider, &postgres::PostgresProvider];

pub fn get_active_services(ctx: &WorkspaceContext) -> Vec<Box<dyn Service + '_>> {
    let mut active = Vec::new();
    for provider in BUILTIN_SERVICES {
        if let Some(service) = provider.get_active_service(ctx) {
            active.push(service);
        }
    }
    active
}

pub fn all_reserved_names() -> Vec<&'static str> {
    let mut names = vec!["base"];
    for provider in BUILTIN_SERVICES {
        names.extend(provider.reserved_names());
    }
    names
}

pub fn run_shell(
    ctx: &WorkspaceContext,
    command: &str,
    env: &BTreeMap<String, String>,
) -> Result<()> {
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
