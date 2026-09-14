use crate::context::WorkspaceContext;
use crate::docker::DockerCompose;
use crate::env_writer::EnvWriter;
use anyhow::{Context, Result, bail};
use colored::Colorize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use std::io::Write;

/// Silently ignores write errors to avoid panics (e.g. EIO when the terminal is closed / SIGHUP),
/// ensuring cleanup and docker teardown can run to completion.
macro_rules! safe_println {
    () => {
        let _ = writeln!(std::io::stdout());
    };
    ($($arg:tt)*) => {
        let _ = writeln!(std::io::stdout(), $($arg)*);
    };
}

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

        safe_println!();
        safe_println!(
            "{}",
            "═══════════════════════════════════════════════════════".green()
        );
        safe_println!(
            "  {} Services are up and running in workspace",
            "ai-igniter:".bold()
        );
        safe_println!("  Workspace:       {}", ctx.workspace_path.display());
        if ctx.root_path != ctx.workspace_path {
            safe_println!("  Root:            {}", ctx.root_path.display());
        }
        safe_println!("  Compose project: {}", ctx.compose_project.cyan());
        safe_println!("  Base port:       {}", ctx.base_port.to_string().yellow());

        let images = ctx.service_images();
        for (name, port) in &ctx.port_allocations {
            let image_info = images
                .get(name)
                .map(|img| format!(" ({img})"))
                .unwrap_or_default();
            safe_println!(
                "  - {:<14} localhost:{}{}",
                format!("{}:", name),
                port,
                image_info.dimmed()
            );
        }
        let interpolated_cmd = dev_command
            .map(|cmd| ctx.interpolate(cmd))
            .transpose()?;

        safe_println!();
        if let Some(cmd) = &interpolated_cmd {
            safe_println!("  Executing dev command: {}", cmd.cyan().bold());
        } else {
            safe_println!("  Keeping services alive in foreground.");
        }
        safe_println!(
            "  Press {} to gracefully stop all services.",
            "Ctrl+C".bold()
        );
        safe_println!(
            "{}",
            "═══════════════════════════════════════════════════════".green()
        );
        safe_println!();

        let mut child = if let Some(ref cmd) = interpolated_cmd {
            let mut command = create_dev_command(ctx, cmd)?;
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

        if shutdown.requested() {
            safe_println!("\n{} Shutdown signal received.", "[dev]".yellow().bold());
        } else if let Some(status) = child_exit_status.filter(|s| !s.success()) {
            safe_println!(
                "\n{} Dev command exited with status: {}",
                "[dev]".yellow().bold(),
                status
            );
        }

        Ok(())
    }
}

/// Returns the environment variables to provide to the dev command:
/// computed from `[env_template]`, with `PORT` and PostgreSQL variables as fallbacks.
pub fn dev_env(ctx: &WorkspaceContext) -> BTreeMap<String, String> {
    let (mut env, _warnings) = EnvWriter::compute_env(ctx);
    env.entry("PORT".to_string())
        .or_insert_with(|| ctx.base_port.to_string());
    if let Some(url) = ctx.template_vars().remove("services.postgres.url") {
        for key in ["DATABASE_URL", "DATABASE_MIGRATION_URL"] {
            env.entry(key.to_string()).or_insert_with(|| url.clone());
        }
    }
    env
}

pub fn create_dev_command(
    ctx: &WorkspaceContext,
    cmd: &str,
) -> Result<std::process::Command> {
    let interpolated = ctx.interpolate(cmd)?;
    let mut command = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", &interpolated]);
        c
    } else {
        let mut c = std::process::Command::new("sh");
        c.args(["-c", &interpolated]);
        c
    };
    command.current_dir(&ctx.workspace_path);
    command.envs(dev_env(ctx));
    Ok(command)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn dev_command_interpolates_and_injects_env() {
        let toml = r#"
name = "test-app"
[env_template]
CUSTOM_VAR = "hello-{{project.name}}"
"#;
        let ctx = crate::context::tests::test_ctx(toml, Some(4123));
        let cmd = create_dev_command(&ctx, "bun run dev -- --port {{ports.base}} $PORT").unwrap();

        let args: Vec<&OsStr> = cmd.get_args().collect();
        if cfg!(windows) {
            assert_eq!(args, &["/C", "bun run dev -- --port 4123 $PORT"]);
        } else {
            assert_eq!(args, &["-c", "bun run dev -- --port 4123 $PORT"]);
        }

        let envs: std::collections::HashMap<&OsStr, Option<&OsStr>> = cmd.get_envs().collect();
        assert_eq!(
            envs.get(OsStr::new("PORT")).and_then(|v| *v),
            Some(OsStr::new("4123"))
        );
        assert_eq!(
            envs.get(OsStr::new("CUSTOM_VAR")).and_then(|v| *v),
            Some(OsStr::new("hello-test-app"))
        );
    }

    #[test]
    fn dev_command_fails_on_unknown_placeholder() {
        let ctx = crate::context::tests::test_ctx("name = \"test-app\"", Some(4123));
        let result = create_dev_command(&ctx, "bun run dev -- --port {{unknown.var}}");
        assert!(result.is_err());
    }
}

