use crate::cli::InitArgs;
use crate::config::*;
use anyhow::{Context, Result, bail};
use colored::Colorize;
use inquire::{Confirm, MultiSelect, Select, Text};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub fn execute_init(args: InitArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("Failed to get current directory")?;
    let target_file = cwd.join(CONFIG_FILE_NAME);

    if target_file.exists() && !args.force {
        if args.non_interactive {
            bail!("{} already exists. Use --force to overwrite it.", CONFIG_FILE_NAME);
        }
        let overwrite = Confirm::new("ai-igniter.toml already exists. Overwrite?")
            .with_default(false)
            .prompt()?;
        if !overwrite {
            println!("Initialization cancelled.");
            return Ok(());
        }
    }

    let default_name = cwd
        .file_name()
        .map(|n| sanitize_name(&n.to_string_lossy()))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "my-project".to_string());

    let raw_name = if let Some(n) = args.name {
        n
    } else if args.non_interactive {
        default_name
    } else {
        Text::new("Project name:")
            .with_initial_value(&default_name)
            .prompt()?
    };
    // The name doubles as database, user and Compose project name
    let project_name = sanitize_name(&raw_name);
    if project_name.is_empty() {
        bail!("Project name {:?} must contain at least one ASCII letter or digit", raw_name);
    }

    let orchestrator_cfg = if args.non_interactive {
        OrchestratorConfig {
            port_env: Some("PASEO_PORT".to_string()),
            root_env: Some("PASEO_SOURCE_CHECKOUT_PATH".to_string()),
        }
    } else {
        let orchestrator_options = vec![
            "Paseo",
            "Conductor",
            "Orca",
            "Custom (specify environment variable names)",
            "Generic / Fallback (WORKSPACE_*)",
        ];
        let chosen = Select::new("Select orchestrator:", orchestrator_options)
            .prompt()?;

        match chosen {
            "Paseo" => OrchestratorConfig {
                port_env: Some("PASEO_PORT".to_string()),
                root_env: Some("PASEO_SOURCE_CHECKOUT_PATH".to_string()),
            },
            "Conductor" => OrchestratorConfig {
                port_env: Some("CONDUCTOR_PORT".to_string()),
                root_env: Some("CONDUCTOR_ROOT_PATH".to_string()),
            },
            "Orca" => OrchestratorConfig {
                port_env: Some("ORCA_PORT".to_string()),
                root_env: Some("ORCA_ROOT_PATH".to_string()),
            },
            "Custom (specify environment variable names)" => {
                let port_var = Text::new("Port env var name:")
                    .with_initial_value("PORT")
                    .prompt()?;
                let root_var = Text::new("Source checkout dir env var name:")
                    .with_initial_value("WORKSPACE_ROOT_PATH")
                    .prompt()?;
                OrchestratorConfig {
                    port_env: Some(port_var),
                    root_env: Some(root_var),
                }
            }
            _ => OrchestratorConfig {
                port_env: Some("WORKSPACE_PORT".to_string()),
                root_env: Some("WORKSPACE_ROOT_PATH".to_string()),
            },
        }
    };

    let mut env_template = BTreeMap::new();
    let mut services = ServicesConfig::default();

    if args.non_interactive {
        for provider in crate::services::BUILTIN_SERVICES {
            if provider.init_default_selected() {
                let templates = provider.prompt_init(&mut services, &project_name, true)?;
                env_template.extend(templates);
            }
        }
    } else {
        let service_options: Vec<&'static str> = crate::services::BUILTIN_SERVICES
            .iter()
            .map(|p| p.init_label())
            .collect();
        let default_indices: Vec<usize> = crate::services::BUILTIN_SERVICES
            .iter()
            .enumerate()
            .filter(|(_, p)| p.init_default_selected())
            .map(|(i, _)| i)
            .collect();

        let chosen = MultiSelect::new("Select services for this project:", service_options)
            .with_default(&default_indices)
            .prompt()?;

        for provider in crate::services::BUILTIN_SERVICES {
            if chosen.contains(&provider.init_label()) {
                let templates = provider.prompt_init(&mut services, &project_name, false)?;
                env_template.extend(templates);
            }
        }
    }

    let config = Config {
        name: project_name,
        base_port: None,
        compose_file: None,
        orchestrator: orchestrator_cfg,
        services,
        env_template,
    };
    config.validate()?;

    let toml_str = toml::to_string_pretty(&config).context("Failed to serialize config to TOML")?;
    fs::write(&target_file, toml_str).with_context(|| format!("Failed to write {:?}", target_file))?;
    ensure_gitignored(&cwd, ".igniter/")?;

    println!();
    println!("{} Created {}", "✓".green().bold(), target_file.display().to_string().cyan());
    println!("{} You can now run:", "Next steps:".bold());
    println!("  {} {}", "•".cyan(), "ai-igniter dev".bold());
    println!("  {} {}", "•".cyan(), "ai-igniter teardown".bold());

    Ok(())
}

/// Appends `entry` to `dir/.gitignore` unless an equivalent pattern is already there.
fn ensure_gitignored(dir: &Path, entry: &str) -> Result<()> {
    let path = dir.join(".gitignore");
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let bare = entry.trim_matches('/');
    if existing.lines().any(|line| line.trim().trim_matches('/') == bare) {
        return Ok(());
    }
    let separator = if existing.is_empty() || existing.ends_with('\n') { "" } else { "\n" };
    fs::write(&path, format!("{existing}{separator}{entry}\n")).with_context(|| format!("Failed to update {:?}", path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_igniter_dir_to_gitignore_once() {
        let dir = std::env::temp_dir().join(format!("ai-igniter-init-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".gitignore"), "node_modules").unwrap();

        ensure_gitignored(&dir, ".igniter/").unwrap();
        ensure_gitignored(&dir, ".igniter/").unwrap();

        assert_eq!(fs::read_to_string(dir.join(".gitignore")).unwrap(), "node_modules\n.igniter/\n");
        fs::remove_dir_all(&dir).unwrap();
    }
}
