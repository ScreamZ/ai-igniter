use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "ai-igniter",
    about = "Lightning-fast, extensible workspace & service orchestrator for AI worktrees (Paseo, Conductor, Orca)",
    version
)]
pub struct Cli {
    /// Workspace / Worktree directory (defaults to the enclosing project, then $WORKSPACE_PATH, $PASEO_WORKTREE_PATH, $CONDUCTOR_WORKSPACE_PATH, $ORCA_WORKSPACE_PATH)
    #[arg(short, long, global = true)]
    pub dir: Option<PathBuf>,

    /// Source checkout directory (defaults to $<orchestrator.root_env>, then the main git checkout)
    #[arg(short, long, global = true)]
    pub root: Option<PathBuf>,

    /// Base port for services & app (defaults to $<orchestrator.port_env>, $WORKSPACE_PORT, $PASEO_PORT, $CONDUCTOR_PORT, config, or a port derived from the workspace path)
    #[arg(short, long, global = true)]
    pub port: Option<u16>,

    /// Path to configuration file (defaults to ai-igniter.toml)
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Interactively initialize ai-igniter for this project (fast service selection)
    Init(InitArgs),

    /// Start workspace services and keep them running (stops Docker when interrupted)
    #[command(alias = "up")]
    Dev(DevArgs),

    /// Teardown workspace: remove containers, volumes, orphans and generated files
    #[command(alias = "archive", alias = "down")]
    Teardown,

    /// Display service health, running containers, and port mappings
    Status,

    /// Compute and display or write workspace environment variables
    Env(EnvArgs),

    /// Check for updates and update ai-igniter to the latest version
    #[command(alias = "upgrade")]
    Update(UpdateArgs),
}

#[derive(Args, Debug)]
pub struct InitArgs {
    /// Project name (defaults to current directory name)
    #[arg(short, long)]
    pub name: Option<String>,

    /// Non-interactive mode using defaults
    #[arg(long)]
    pub non_interactive: bool,

    /// Overwrite an existing ai-igniter.toml without asking
    #[arg(long)]
    pub force: bool,
}

#[derive(Args, Debug)]
pub struct DevArgs {
    /// Wipe volumes and recreate services from scratch before starting
    #[arg(long)]
    pub reset: bool,

    /// Do not run the dev_command configured in ai-igniter.toml
    #[arg(long)]
    pub no_command: bool,

    /// Optional command to run instead of dev_command from ai-igniter.toml (e.g. `ai-igniter dev -- bun run dev`)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub command: Vec<String>,
}

#[derive(Args, Debug)]
pub struct EnvArgs {
    /// Write the evaluated variables to env_file
    #[arg(short, long)]
    pub write: bool,
}

#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Only check if an update is available without downloading or replacing the binary
    #[arg(long)]
    pub check: bool,

    /// Force update via cargo (`cargo install ai-igniter --force`) instead of GitHub Releases
    #[arg(long)]
    pub cargo: bool,
}
