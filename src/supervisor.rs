use crate::context::WorkspaceContext;
use crate::docker::DockerCompose;
use anyhow::{Context, Result, bail};
use colored::Colorize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Set on SIGINT / SIGTERM / SIGHUP. Installed before any Docker work so an interrupt always stops the services.
pub struct ShutdownSignal(Arc<AtomicBool>);

impl ShutdownSignal {
    pub fn install() -> Result<Self> {
        let flag = Arc::new(AtomicBool::new(false));
        let handler_flag = flag.clone();
        ctrlc::set_handler(move || handler_flag.store(true, Ordering::SeqCst))
            .context("Failed to install signal handler")?;
        Ok(Self(flag))
    }

    pub fn requested(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

pub struct Supervisor;

impl Supervisor {
    /// Blocks until a shutdown signal, or fails if a service that was running stops on its own.
    pub fn run_dev(ctx: &WorkspaceContext, compose: &DockerCompose<'_>, shutdown: &ShutdownSignal) -> Result<()> {
        let expected = compose.running_services()?;

        println!();
        println!("{}", "═══════════════════════════════════════════════════════".green());
        println!("  {} Services are up and running in workspace", "ai-igniter:".bold());
        println!("  Workspace:       {}", ctx.workspace_path.display());
        if ctx.root_path != ctx.workspace_path {
            println!("  Root:            {}", ctx.root_path.display());
        }
        println!("  Compose project: {}", ctx.compose_project.cyan());
        println!("  Base port:       {}", ctx.base_port.to_string().yellow());

        let images = ctx.service_images();
        for (name, port) in &ctx.port_allocations {
            let image_info = images
                .get(name)
                .map(|img| format!(" ({img})"))
                .unwrap_or_default();
            println!("  - {:<14} localhost:{}{}", format!("{}:", name), port, image_info.dimmed());
        }
        println!();
        println!("  Keeping services alive in foreground.");
        println!("  Press {} to gracefully stop all services.", "Ctrl+C".bold());
        println!("{}", "═══════════════════════════════════════════════════════".green());
        println!();

        let mut last_check = Instant::now();
        while !shutdown.requested() {
            thread::sleep(Duration::from_millis(200));
            if last_check.elapsed() < HEALTH_CHECK_INTERVAL {
                continue;
            }
            last_check = Instant::now();
            // A transient `docker compose ps` failure is not a service failure
            let Ok(running) = compose.running_services() else { continue };
            let stopped: Vec<&str> = expected.difference(&running).map(String::as_str).collect();
            if !stopped.is_empty() && !shutdown.requested() {
                bail!("Service(s) stopped unexpectedly: {}", stopped.join(", "));
            }
        }

        println!("\n{} Shutdown signal received.", "[dev]".yellow().bold());
        Ok(())
    }
}
