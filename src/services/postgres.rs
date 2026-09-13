use super::{Service, ServiceProvider, run_shell};
use crate::config::ServicesConfig;
use crate::context::WorkspaceContext;
use crate::context::url_encode;
use crate::docker::DockerCompose;
use crate::env_writer::EnvWriter;
use anyhow::{Context, Result};
use colored::Colorize;
use inquire::Text;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// Stored as the database comment once seeded; it disappears with the volume, so fresh databases get seeded again.
const SEED_MARKER: &str = "ai-igniter:seeded";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostgresConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_pg_offset")]
    pub port_offset: u16,
    #[serde(default = "default_pg_image")]
    pub image: String,
    pub database: String,
    pub user: String,
    pub password: String,
    pub e2e_database: Option<String>,
    pub migrate_command: Option<String>,
    pub seed_command: Option<String>,
    /// SQL returning a count; the seed is skipped when it is > 0. Without it, the seed runs once per fresh volume.
    pub seed_check_sql: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_pg_offset() -> u16 {
    1
}

fn default_pg_image() -> String {
    "postgres:16".to_string()
}

pub struct PostgresProvider;

impl ServiceProvider for PostgresProvider {
    fn name(&self) -> &'static str {
        "postgres"
    }

    fn init_label(&self) -> &'static str {
        "PostgreSQL (with migrations & seeds)"
    }

    fn port_offsets(&self, services: &ServicesConfig) -> Vec<(String, u16)> {
        if let Some(pg) = services.postgres.as_ref().filter(|c| c.enabled) {
            vec![("postgres".to_string(), pg.port_offset)]
        } else {
            vec![]
        }
    }

    fn prompt_init(
        &self,
        services: &mut ServicesConfig,
        project_name: &str,
        non_interactive: bool,
    ) -> Result<BTreeMap<String, String>> {
        let (migrate_command, seed_command) = if non_interactive {
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

        services.postgres = Some(PostgresConfig {
            enabled: true,
            port_offset: 1,
            image: default_pg_image(),
            database: project_name.to_string(),
            user: project_name.to_string(),
            password: project_name.to_string(),
            e2e_database: Some(format!("{}_e2e", project_name)),
            migrate_command,
            seed_command,
            seed_check_sql: None,
        });

        let mut templates = BTreeMap::new();
        templates.insert("DATABASE_URL".to_string(), "{{services.postgres.url}}".to_string());
        templates.insert("DATABASE_MIGRATION_URL".to_string(), "{{services.postgres.url}}".to_string());
        templates.insert("E2E_DATABASE_URL".to_string(), "{{services.postgres.e2e_url}}".to_string());
        Ok(templates)
    }

    fn contribute_compose(
        &self,
        ctx: &WorkspaceContext,
        services: &mut Map<String, Value>,
        volumes: &mut Map<String, Value>,
    ) {
        if let Some(pg) = ctx.config.services.postgres.as_ref().filter(|c| c.enabled) {
            let port = ctx.port_allocations["postgres"];
            services.insert(
                "postgres".into(),
                json!({
                    "image": pg.image,
                    "environment": {
                        "POSTGRES_DB": pg.database,
                        "POSTGRES_USER": pg.user,
                        "POSTGRES_PASSWORD": pg.password,
                    },
                    // -h forces TCP: the entrypoint's temporary init server only listens on the Unix socket
                    "healthcheck": {
                        "test": ["CMD", "pg_isready", "-h", "127.0.0.1", "-U", pg.user, "-d", pg.database],
                        "interval": "2s",
                        "timeout": "5s",
                        "retries": 30,
                    },
                    "ports": [format!("{}:5432", port)],
                    "volumes": ["postgres-data:/var/lib/postgresql/data"],
                }),
            );
            volumes.insert("postgres-data".into(), json!({}));
        }
    }

    fn contribute_template_vars(&self, ctx: &WorkspaceContext, vars: &mut BTreeMap<String, String>) {
        if let Some(pg) = ctx.config.services.postgres.as_ref().filter(|c| c.enabled) {
            let port = ctx.port_allocations["postgres"];
            let url = |db: &str| {
                format!(
                    "postgresql://{}:{}@127.0.0.1:{port}/{}",
                    url_encode(&pg.user),
                    url_encode(&pg.password),
                    url_encode(db)
                )
            };
            vars.insert("services.postgres.port".into(), port.to_string());
            vars.insert("services.postgres.host".into(), "127.0.0.1".into());
            vars.insert("services.postgres.user".into(), pg.user.clone());
            vars.insert("services.postgres.password".into(), pg.password.clone());
            vars.insert("services.postgres.database".into(), pg.database.clone());
            vars.insert("services.postgres.url".into(), url(&pg.database));
            if let Some(e2e) = &pg.e2e_database {
                vars.insert("services.postgres.e2e_database".into(), e2e.clone());
                vars.insert("services.postgres.e2e_url".into(), url(e2e));
            }
        }
    }

    fn get_active_service<'a>(&self, ctx: &'a WorkspaceContext) -> Option<Box<dyn Service + 'a>> {
        ctx.config.services.postgres.as_ref().filter(|c| c.enabled).map(|pg| {
            let s: Box<dyn Service + 'a> = Box::new(PostgresService { config: pg });
            s
        })
    }
}

pub struct PostgresService<'a> {
    pub config: &'a PostgresConfig,
}

impl Service for PostgresService<'_> {
    fn name(&self) -> &str {
        "postgres"
    }

    fn post_start(&self, ctx: &WorkspaceContext, compose: &DockerCompose<'_>) -> Result<()> {
        if let Some(e2e_db) = &self.config.e2e_database {
            self.ensure_database(compose, e2e_db)?;
        }

        let env = command_env(ctx);

        if let Some(migrate_cmd) = &self.config.migrate_command {
            println!("{} Running database migrations...", "[postgres]".blue().bold());
            run_shell(ctx, &ctx.interpolate(migrate_cmd)?, &env).context("Migration failed")?;
        }

        if let Some(seed_cmd) = &self.config.seed_command {
            self.seed(ctx, compose, seed_cmd, &env)?;
        }

        Ok(())
    }
}

impl PostgresService<'_> {
    fn psql(&self, compose: &DockerCompose<'_>, sql: &str) -> Result<String> {
        compose.exec_checked(
            "postgres",
            &["psql", "-U", &self.config.user, "-d", &self.config.database, "-v", "ON_ERROR_STOP=1", "-tAc", sql],
        )
    }

    fn ensure_database(&self, compose: &DockerCompose<'_>, name: &str) -> Result<()> {
        let exists = self.psql(compose, &format!("SELECT 1 FROM pg_database WHERE datname = {}", quote_literal(name)))?;
        if exists != "1" {
            println!("{} Creating secondary database '{}'...", "[postgres]".blue().bold(), name.cyan());
            self.psql(compose, &format!("CREATE DATABASE {}", quote_ident(name)))?;
        }
        Ok(())
    }

    fn seed(&self, ctx: &WorkspaceContext, compose: &DockerCompose<'_>, seed_cmd: &str, env: &BTreeMap<String, String>) -> Result<()> {
        let already_seeded = match &self.config.seed_check_sql {
            Some(sql) => self
                .psql(compose, sql)
                .context("seed_check_sql failed")?
                .parse::<i64>()
                .is_ok_and(|count| count > 0),
            None => {
                let marker = self.psql(
                    compose,
                    "SELECT shobj_description(oid, 'pg_database') FROM pg_database WHERE datname = current_database()",
                )?;
                marker == SEED_MARKER
            }
        };

        if already_seeded {
            println!("{} Database already seeded, skipping seed.", "[postgres]".blue().bold());
            return Ok(());
        }

        println!("{} Seeding fresh database...", "[postgres]".blue().bold());
        if let Err(e) = run_shell(ctx, &ctx.interpolate(seed_cmd)?, env) {
            eprintln!("{} Warning: seed failed, will retry next start: {:#}", "[postgres]".yellow().bold(), e);
            return Ok(());
        }

        let comment = format!("COMMENT ON DATABASE {} IS {}", quote_ident(&self.config.database), quote_literal(SEED_MARKER));
        self.psql(compose, &comment)?;
        Ok(())
    }
}

fn non_empty(input: String) -> Option<String> {
    let trimmed = input.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Migration and seed commands see the same variables as the generated `.env`.
fn command_env(ctx: &WorkspaceContext) -> BTreeMap<String, String> {
    let (mut env, _warnings) = EnvWriter::compute_env(ctx);
    if let Some(url) = ctx.template_vars().remove("services.postgres.url") {
        for key in ["DATABASE_URL", "DATABASE_MIGRATION_URL"] {
            env.entry(key.to_string()).or_insert_with(|| url.clone());
        }
    }
    env
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn quotes_sql_identifiers_and_literals() {
        assert_eq!(quote_ident("ai-tools_e2e"), "\"ai-tools_e2e\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(quote_literal("o'brien"), "'o''brien'");
    }
}
