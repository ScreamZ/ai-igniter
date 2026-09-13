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

    let (enable_pg, enable_s3, enable_redis) = if args.non_interactive {
        (true, true, false)
    } else {
        let service_options = vec![
            "PostgreSQL (with migrations & seeds)",
            "Garage S3 (local S3 + website hosting)",
            "Redis",
        ];
        let chosen = MultiSelect::new("Select services for this project:", service_options)
            .with_default(&[0, 1])
            .prompt()?;

        (
            chosen.iter().any(|s| s.contains("PostgreSQL")),
            chosen.iter().any(|s| s.contains("Garage")),
            chosen.iter().any(|s| s.contains("Redis")),
        )
    };

    // Auto-generate all service details based on project name and dynamic conventions
    let postgres_cfg = if enable_pg {
        let (migrate_command, seed_command) = if args.non_interactive {
            (Some("bun run db:migrate".to_string()), Some("bun run db:seed".to_string()))
        } else {
            let migrate_input = Text::new("PostgreSQL migration command (empty to disable):")
                .with_initial_value("bun run db:migrate")
                .with_help_message("Edit or clear to disable")
                .prompt()?;
            let seed_input = Text::new("PostgreSQL seed command (empty to disable):")
                .with_initial_value("bun run db:seed")
                .with_help_message("Runs once per fresh database volume. Edit or clear to disable")
                .prompt()?;
            (non_empty(migrate_input), non_empty(seed_input))
        };

        Some(PostgresConfig {
            enabled: true,
            port_offset: 1,
            image: "postgres:16".to_string(),
            database: project_name.clone(),
            user: project_name.clone(),
            password: project_name.clone(),
            e2e_database: Some(format!("{}_e2e", project_name)),
            migrate_command,
            seed_command,
            seed_check_sql: None,
        })
    } else {
        None
    };

    let (garage_cfg, s3_bucket_name) = if enable_s3 {
        let default_bucket = format!("{}-assets", project_name);
        let bucket_name = if args.non_interactive {
            default_bucket.clone()
        } else {
            Text::new("S3 bucket name:")
                .with_initial_value(&default_bucket)
                .with_help_message("Default bucket created and exposed for web hosting")
                .prompt()?
        };
        let final_bucket = non_empty(bucket_name).unwrap_or(default_bucket);

        (
            Some(GarageConfig {
                enabled: true,
                port_offset: 3,
                web_port_offset: 4,
                image: "dxflrs/garage:v2.4.1".to_string(),
                access_key: format!("{}-local-access-key", project_name),
                secret_key: format!("{}-local-secret-key-change-me", project_name),
                rpc_secret: None,
                buckets: vec![final_bucket.clone()],
                website_buckets: vec![final_bucket.clone()],
                website_root_domain: ".web.localhost".to_string(),
            }),
            Some(final_bucket),
        )
    } else {
        (None, None)
    };

    let redis_cfg = if enable_redis {
        Some(RedisConfig {
            enabled: true,
            port_offset: 5,
            image: "redis:7-alpine".to_string(),
            password: None,
        })
    } else {
        None
    };

    // Placeholders keep the .env in sync when credentials or ports change in the config
    let mut env_template = BTreeMap::new();
    let mut template = |key: &str, value: String| {
        env_template.insert(key.to_string(), value);
    };
    if enable_pg {
        template("DATABASE_URL", "{{services.postgres.url}}".to_string());
        template("DATABASE_MIGRATION_URL", "{{services.postgres.url}}".to_string());
        template("E2E_DATABASE_URL", "{{services.postgres.e2e_url}}".to_string());
    }
    if let Some(bucket) = s3_bucket_name {
        template("S3_ENDPOINT", "{{services.garage.endpoint}}".to_string());
        template("S3_ACCESS_KEY_ID", "{{services.garage.access_key}}".to_string());
        template("S3_SECRET_ACCESS_KEY", "{{services.garage.secret_key}}".to_string());
        template("S3_REGION", "{{services.garage.region}}".to_string());
        template("S3_BUCKET", bucket.clone());
        template(
            "S3_PUBLIC_URL",
            format!("http://{bucket}{{{{services.garage.website_root_domain}}}}:{{{{services.garage.web_port}}}}"),
        );
    }
    if enable_redis {
        template("REDIS_URL", "{{services.redis.url}}".to_string());
    }

    let config = Config {
        name: project_name,
        base_port: None,
        compose_file: None,
        orchestrator: orchestrator_cfg,
        services: ServicesConfig {
            postgres: postgres_cfg,
            garage: garage_cfg,
            redis: redis_cfg,
            custom: Default::default(),
        },
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

fn non_empty(input: String) -> Option<String> {
    let trimmed = input.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
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
