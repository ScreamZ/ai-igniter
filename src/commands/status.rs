use crate::context::WorkspaceContext;
use crate::docker::DockerCompose;
use anyhow::Result;
use colored::Colorize;

pub fn execute_status(ctx: &WorkspaceContext) -> Result<()> {
    println!("{}", "═══════════════════════════════════════════════════════".cyan());
    println!("  {} Workspace Status", "ai-igniter:".bold());
    println!("{}", "═══════════════════════════════════════════════════════".cyan());
    println!("  Project Name:     {}", ctx.config.name.bold());
    println!("  Compose Project:  {}", ctx.compose_project.yellow());
    println!("  Workspace Path:   {}", ctx.workspace_path.display());
    println!("  Root Path:        {}", ctx.root_path.display());
    println!("  Config Path:      {}", ctx.config_path.display());
    println!("  Base Port:        {}", ctx.base_port);
    println!();
    println!("  Allocated Ports:");
    for (name, port) in &ctx.port_allocations {
        println!("    - {:<16} : {}", name.bold(), port.to_string().cyan());
    }
    println!();

    let compose = DockerCompose::new(ctx)?;
    match compose.ps() {
        Ok(containers) if containers.is_empty() => {
            println!("  Containers: {}", "No containers for this project".yellow());
        }
        Ok(containers) => {
            println!("  Containers:");
            for c in &containers {
                let field = |key: &str| c[key].as_str().unwrap_or("").to_string();
                let state = field("State");
                let state_colored = if state == "running" { state.green() } else { state.red() };
                println!(
                    "    - {:<14} (service: {:<10}) [{}] {} {}",
                    field("Name").cyan(),
                    field("Service"),
                    state_colored,
                    field("Health").yellow(),
                    field("Status").dimmed()
                );
            }
        }
        Err(e) => {
            println!("  Containers: {} ({:#})", "Could not inspect Docker containers".red(), e);
        }
    }

    println!("{}", "═══════════════════════════════════════════════════════".cyan());
    Ok(())
}
