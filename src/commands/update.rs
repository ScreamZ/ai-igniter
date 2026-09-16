use anyhow::{Context, Result, bail};
use colored::Colorize;
use self_update::cargo_crate_version;
use std::process::Command;

const REPO_OWNER: &str = "ScreamZ";
const REPO_NAME: &str = "ai-igniter";
const BIN_NAME: &str = "ai-igniter";

/// Target triple string expected in the release asset name
fn current_target_triple() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "aarch64-apple-darwin"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "x86_64-apple-darwin"
    }
    #[cfg(all(target_os = "linux", target_env = "musl"))]
    {
        "x86_64-unknown-linux-musl"
    }
    #[cfg(all(target_os = "linux", not(target_env = "musl")))]
    {
        "x86_64-unknown-linux-gnu"
    }
    #[cfg(target_os = "windows")]
    {
        "x86_64-pc-windows-msvc"
    }
    #[cfg(not(any(
        all(
            target_os = "macos",
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        target_os = "linux",
        target_os = "windows"
    )))]
    {
        "unsupported"
    }
}

pub fn execute_update(check_only: bool, force_cargo: bool) -> Result<()> {
    let current_v = cargo_crate_version!();
    println!(
        "{} Checking for updates (current version: {})...",
        "[update]".cyan().bold(),
        current_v.bold()
    );

    if force_cargo {
        if check_only {
            println!("{} Querying crates.io / cargo...", "[info]".blue().bold());
        }
        return update_via_cargo(check_only);
    }

    // Try GitHub Releases first (fastest, pre-compiled binary replacement)
    match update_via_github(check_only) {
        Ok(true) => Ok(()),
        Ok(false) => {
            // Already up to date via GitHub
            Ok(())
        }
        Err(e) => {
            println!(
                "{} GitHub release update unavailable: {:#}",
                "[warn]".yellow().bold(),
                e
            );
            println!(
                "{} Falling back to 'cargo install'...",
                "[info]".blue().bold()
            );
            update_via_cargo(check_only)
        }
    }
}

fn update_via_github(check_only: bool) -> Result<bool> {
    let target = current_target_triple();
    let current_v = cargo_crate_version!();

    let mut builder = self_update::backends::github::Update::configure();
    builder
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name(BIN_NAME)
        .current_version(current_v)
        .target(target)
        .show_download_progress(true)
        .show_output(false);

    let updater = builder
        .build()
        .context("Failed to configure GitHub updater")?;

    if check_only {
        let releases = updater
            .get_latest_release()
            .context("Failed to fetch latest release")?;
        let latest = match releases.latest() {
            Some(r) => r,
            None => {
                println!(
                    "{} ai-igniter is already up to date ({})",
                    "✓".green(),
                    current_v.bold()
                );
                return Ok(false);
            }
        };

        let latest_ver = latest.version();
        if self_update::version::bump_is_greater(current_v, latest_ver).unwrap_or(false) {
            println!(
                "{} A new version is available: {} -> {}",
                "✨".green(),
                current_v.yellow(),
                latest_ver.green().bold()
            );
            println!("Run {} to upgrade.", "ai-igniter update".bold().cyan());
            return Ok(true);
        } else {
            println!(
                "{} ai-igniter is already up to date ({})",
                "✓".green(),
                current_v.bold()
            );
            return Ok(false);
        }
    }

    let status = updater
        .update()
        .context("Failed to perform binary update")?;
    if status.is_updated() {
        println!(
            "{} Successfully updated ai-igniter to version {}!",
            "✨".green().bold(),
            status.version().green().bold()
        );
        Ok(true)
    } else {
        println!(
            "{} ai-igniter is already up to date ({})",
            "✓".green(),
            status.version().bold()
        );
        Ok(false)
    }
}

fn update_via_cargo(check_only: bool) -> Result<()> {
    // Check if cargo is available in PATH
    let cargo_check = Command::new("cargo").arg("--version").output();
    if cargo_check.is_err() {
        bail!(
            "'cargo' executable not found in PATH. Please install Rust or download pre-built binaries from GitHub Releases."
        );
    }

    if check_only {
        println!(
            "{} To update via cargo, run: {}",
            "[info]".blue().bold(),
            "cargo install ai-igniter --force".cyan().bold()
        );
        return Ok(());
    }

    println!(
        "{} Running {}...",
        "[cargo]".cyan().bold(),
        "cargo install ai-igniter --force".bold()
    );

    let mut child = Command::new("cargo")
        .args(["install", "ai-igniter", "--force"])
        .spawn()
        .context("Failed to spawn 'cargo install'")?;

    let exit_status = child.wait().context("Failed waiting for 'cargo install'")?;
    if exit_status.success() {
        println!(
            "{} Successfully updated ai-igniter via cargo!",
            "✨".green().bold()
        );
        Ok(())
    } else {
        bail!("'cargo install' exited with status {}", exit_status);
    }
}

// ---------------------------------------------------------------------------
// Non-blocking background update check & notification banner
// ---------------------------------------------------------------------------

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

const CACHE_TTL_SECS: u64 = 24 * 60 * 60; // 24 hours

#[derive(Serialize, Deserialize, Debug)]
struct UpdateCache {
    last_checked_at: u64,
    latest_version: String,
}

fn cache_file_path() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    Some(
        PathBuf::from(home)
            .join(".ai-igniter")
            .join("update_cache.json"),
    )
}

fn read_cache() -> Option<UpdateCache> {
    let path = cache_file_path()?;
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn write_cache(latest_version: &str) {
    if let Some(path) = cache_file_path() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let cache = UpdateCache {
            last_checked_at: now,
            latest_version: latest_version.to_string(),
        };
        if let Ok(json) = serde_json::to_string(&cache) {
            let _ = fs::write(path, json);
        }
    }
}

/// Handle background update checking in a detached thread and return a guard
/// that prints a notification banner at command exit if a newer version is found.
pub struct UpdateNotifierGuard {
    newer_version: Option<String>,
}

impl Drop for UpdateNotifierGuard {
    fn drop(&mut self) {
        if let Some(ref new_ver) = self.newer_version {
            // Only print if stderr is an interactive terminal to never break piping
            if std::io::stderr().is_terminal() {
                let current_v = cargo_crate_version!();
                eprintln!();
                eprintln!(
                    "{}",
                    "╭─────────────────────────────────────────────────────────────╮".yellow()
                );
                eprintln!(
                    "{}   Update available: {} {} {}",
                    "│".yellow(),
                    current_v.dimmed(),
                    "→".cyan(),
                    new_ver.green().bold()
                );
                eprintln!(
                    "{}   Run {} to upgrade to the latest version    {}",
                    "│".yellow(),
                    "ai-igniter update".bold().cyan(),
                    "│".yellow()
                );
                eprintln!(
                    "{}",
                    "╰─────────────────────────────────────────────────────────────╯".yellow()
                );
                eprintln!();
            }
        }
    }
}

pub fn spawn_background_update_checker() -> UpdateNotifierGuard {
    let current_v = cargo_crate_version!();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let cache = read_cache();
    let mut newer_version = None;

    let should_fetch = match &cache {
        Some(c) => {
            if self_update::version::bump_is_greater(current_v, &c.latest_version).unwrap_or(false)
            {
                newer_version = Some(c.latest_version.clone());
            }
            now.saturating_sub(c.last_checked_at) >= CACHE_TTL_SECS
        }
        None => true,
    };

    if should_fetch {
        // Spawn a background thread to update the cache asynchronously
        thread::spawn(move || {
            let target = current_target_triple();
            let mut builder = self_update::backends::github::Update::configure();
            builder
                .repo_owner(REPO_OWNER)
                .repo_name(REPO_NAME)
                .bin_name(BIN_NAME)
                .current_version(current_v)
                .target(target)
                .show_download_progress(false)
                .show_output(false);

            if let Ok(updater) = builder.build() {
                if let Ok(releases) = updater.get_latest_release() {
                    if let Some(latest) = releases.latest() {
                        write_cache(latest.version());
                    }
                }
            }
        });
    }

    UpdateNotifierGuard { newer_version }
}
