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
    /// Blocks until a shutdown signal or the dev command finishes, or fails if a service stops on its own.
    pub fn run_dev(
        ctx: &WorkspaceContext,
        compose: &DockerCompose<'_>,
        shutdown: &ShutdownSignal,
        dev_command: Option<&str>,
    ) -> Result<()> {
        let expected = compose.running_services()?;

        println!();
        println!(
            "{}",
            "═══════════════════════════════════════════════════════".green()
        );
        println!(
            "  {} Services are up and running in workspace",
            "ai-igniter:".bold()
        );
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
            println!(
                "  - {:<14} localhost:{}{}",
                format!("{}:", name),
                port,
                image_info.dimmed()
            );
        }
        println!();
        if let Some(cmd) = dev_command {
            println!("  Executing dev command: {}", cmd.cyan().bold());
        } else {
            println!("  Keeping services alive in foreground.");
        }
        println!(
            "  Press {} to gracefully stop all services.",
            "Ctrl+C".bold()
        );
        println!(
            "{}",
            "═══════════════════════════════════════════════════════".green()
        );
        println!();

        let mut child = if let Some(cmd) = dev_command {
            let mut command = if cfg!(windows) {
                let mut c = std::process::Command::new("cmd");
                c.args(["/C", cmd]);
                c
            } else {
                let mut c = std::process::Command::new("sh");
                c.args(["-c", cmd]);
                c
            };
            command.current_dir(&ctx.workspace_path);
            let spawned = command
                .spawn()
                .with_context(|| format!("Failed to spawn dev command: '{cmd}'"))?;
            Some(spawned)
        } else {
            None
        };

        let mut last_check = Instant::now();
        let mut child_exit_status = None;

        while !shutdown.requested() {
            if let Some(ref mut c) = child {
                match c.try_wait() {
                    Ok(Some(status)) => {
                        child_exit_status = Some(status);
                        break;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        eprintln!(
                            "{} Error checking dev command status: {:#}",
                            "[dev]".yellow().bold(),
                            e
                        );
                        break;
                    }
                }
            }

            thread::sleep(Duration::from_millis(100));
            if last_check.elapsed() < HEALTH_CHECK_INTERVAL {
                continue;
            }
            last_check = Instant::now();
            // A transient `docker compose ps` failure is not a service failure
            let Ok(running) = compose.running_services() else {
                continue;
            };
            let stopped: Vec<&str> = expected.difference(&running).map(String::as_str).collect();
            if !stopped.is_empty() && !shutdown.requested() {
                if let Some(ref mut c) = child {
                    let _ = c.kill();
                }
                bail!("Service(s) stopped unexpectedly: {}", stopped.join(", "));
            }
        }

        if let (Some(mut c), None) = (child, child_exit_status) {
            // Terminate child if still running
                #[cfg(unix)]
                unsafe {
                    let pid = c.id() as i32;
                    libc_kill(pid, 15); // SIGTERM
                }
                // Wait briefly for graceful exit, otherwise kill
                let grace_start = Instant::now();
                let mut terminated = false;
                while grace_start.elapsed() < Duration::from_millis(1500) {
                    if let Ok(Some(_)) = c.try_wait() {
                        terminated = true;
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                if !terminated {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
        }

        if shutdown.requested() {
            println!("\n{} Shutdown signal received.", "[dev]".yellow().bold());
        } else if let Some(status) = child_exit_status.filter(|s| !s.success()) {
            println!(
                "\n{} Dev command exited with status: {}",
                "[dev]".yellow().bold(),
                status
            );
        }

        Ok(())
    }
}

#[cfg(unix)]
unsafe fn libc_kill(pid: i32, sig: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe {
        kill(pid, sig);
    }
}
