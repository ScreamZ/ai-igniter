use super::{Service, ServiceProvider, run_shell};
use crate::config::ServicesConfig;
use crate::context::WorkspaceContext;
use crate::context::url_encode;
use crate::docker::DockerCompose;
use crate::env_writer::EnvWriter;
use anyhow::{Context, Result};
use colored::Colorize;
use inquire::{Confirm, Text};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// Stored as the database comment once seeded; it disappears with the volume, so fresh databases get seeded again.
const SEED_MARKER: &str = "ai-igniter:seeded";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NeonProxyConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_neon_proxy_offset")]
    pub port_offset: u16,
    #[serde(default = "default_neon_proxy_image")]
    pub image: String,
}

impl Default for NeonProxyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            port_offset: default_neon_proxy_offset(),
            image: default_neon_proxy_image(),
        }
    }
}

fn default_neon_proxy_offset() -> u16 {
    2
}

fn default_neon_proxy_image() -> String {
    "ghcr.io/timowilhelm/local-neon-http-proxy:main".to_string()
}

fn deserialize_neon_proxy<'de, D>(deserializer: D) -> Result<Option<NeonProxyConfig>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Helper {
        Bool(bool),
        Config(NeonProxyConfig),
    }

    match Option::<Helper>::deserialize(deserializer)? {
        Some(Helper::Bool(true)) => Ok(Some(NeonProxyConfig::default())),
        Some(Helper::Bool(false)) => Ok(Some(NeonProxyConfig {
            enabled: false,
            ..Default::default()
        })),
        Some(Helper::Config(cfg)) => Ok(Some(cfg)),
        None => Ok(None),
    }
}

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
    #[serde(default, deserialize_with = "deserialize_neon_proxy", skip_serializing_if = "Option::is_none")]
    pub neon_proxy: Option<NeonProxyConfig>,
}

impl PostgresConfig {
    pub fn neon_proxy(&self) -> Option<&NeonProxyConfig> {
        self.neon_proxy.as_ref().filter(|c| c.enabled)
    }
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

    fn reserved_names(&self) -> Vec<&'static str> {
        vec!["postgres", "postgres_neon", "neon-proxy"]
    }

    fn port_offsets(&self, services: &ServicesConfig) -> Vec<(String, u16)> {
        if let Some(pg) = services.postgres.as_ref().filter(|c| c.enabled) {
            let mut offsets = vec![("postgres".to_string(), pg.port_offset)];
            if let Some(neon) = pg.neon_proxy() {
                offsets.push(("postgres_neon".to_string(), neon.port_offset));
            }
            offsets
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
        let (migrate_command, seed_command, enable_neon) = if non_interactive {
            (Some("bun run db:migrate".to_string()), Some("bun run db:seed".to_string()), false)
        } else {
            let migrate_input = Text::new("PostgreSQL migration command (empty to disable):")
                .with_initial_value("bun run db:migrate")
                .with_help_message("Edit or clear to disable")
                .prompt()?;
            let seed_input = Text::new("PostgreSQL seed command (empty to disable):")
                .with_initial_value("bun run db:seed")
                .with_help_message("Runs once per fresh database volume. Edit or clear to disable")
                .prompt()?;
            let neon_input = Confirm::new("Enable Neon HTTP proxy (local-neon-http-proxy)?")
                .with_default(false)
                .with_help_message("Exposes PostgreSQL over HTTP on port offset 2 for serverless drivers")
                .prompt()?;
            (non_empty(migrate_input), non_empty(seed_input), neon_input)
        };

        let neon_proxy = if enable_neon {
            Some(NeonProxyConfig::default())
        } else {
            None
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
            neon_proxy,
        });

        let mut templates = BTreeMap::new();
        if enable_neon {
            templates.insert("DATABASE_URL".to_string(), "{{services.postgres.neon_url}}".to_string());
            templates.insert("DATABASE_MIGRATION_URL".to_string(), "{{services.postgres.url}}".to_string());
            templates.insert("E2E_DATABASE_URL".to_string(), "{{services.postgres.neon_e2e_url}}".to_string());
        } else {
            templates.insert("DATABASE_URL".to_string(), "{{services.postgres.url}}".to_string());
            templates.insert("DATABASE_MIGRATION_URL".to_string(), "{{services.postgres.url}}".to_string());
            templates.insert("E2E_DATABASE_URL".to_string(), "{{services.postgres.e2e_url}}".to_string());
        }
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

            if let Some(neon) = pg.neon_proxy() {
                let neon_port = ctx.port_allocations["postgres_neon"];
                let connection_string = format!(
                    "postgresql://{}:{}@postgres:5432/{}",
                    url_encode(&pg.user),
                    url_encode(&pg.password),
                    url_encode(&pg.database)
                );
                services.insert(
                    "neon-proxy".into(),
                    json!({
                        "image": neon.image,
                        "environment": {
                            "PG_CONNECTION_STRING": connection_string,
                        },
                        "ports": [format!("{}:4444", neon_port)],
                        "depends_on": {
                            "postgres": {
                                "condition": "service_healthy",
                            },
                        },
                    }),
                );
            }
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

            if let Some(_neon) = pg.neon_proxy() {
                let neon_port = ctx.port_allocations["postgres_neon"];
                let neon_url = |db: &str| {
                    format!(
                        "postgresql://{}:{}@127.0.0.1:{neon_port}/{}",
                        url_encode(&pg.user),
                        url_encode(&pg.password),
                        url_encode(db)
                    )
                };
                vars.insert("services.postgres.neon_port".into(), neon_port.to_string());
                vars.insert("services.postgres.neon_url".into(), neon_url(&pg.database));
                if let Some(e2e) = &pg.e2e_database {
                    vars.insert("services.postgres.neon_e2e_url".into(), neon_url(e2e));
                }
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

    #[test]
    fn deserializes_neon_proxy_bool_and_table() {
        let bool_true: PostgresConfig = toml::from_str(
            r#"
database = "db"
user = "u"
password = "p"
neon_proxy = true
"#,
        )
        .unwrap();
        let neon = bool_true.neon_proxy().unwrap();
        assert!(neon.enabled);
        assert_eq!(neon.port_offset, 2);
        assert_eq!(neon.image, "ghcr.io/timowilhelm/local-neon-http-proxy:main");

        let bool_false: PostgresConfig = toml::from_str(
            r#"
database = "db"
user = "u"
password = "p"
neon_proxy = false
"#,
        )
        .unwrap();
        assert!(bool_false.neon_proxy().is_none());

        let table: PostgresConfig = toml::from_str(
            r#"
database = "db"
user = "u"
password = "p"
[neon_proxy]
port_offset = 5
image = "custom/neon-proxy:v1"
"#,
        )
        .unwrap();
        let neon = table.neon_proxy().unwrap();
        assert!(neon.enabled);
        assert_eq!(neon.port_offset, 5);
        assert_eq!(neon.image, "custom/neon-proxy:v1");

        let omitted: PostgresConfig = toml::from_str(
            r#"
database = "db"
user = "u"
password = "p"
"#,
        )
        .unwrap();
        assert!(omitted.neon_proxy().is_none());
    }

    #[test]
    fn template_vars_include_neon_urls_with_postgres_scheme() {
        let toml = r#"
name = "neon-app"
[services.postgres]
database = "neon-app"
user = "neon-user"
password = "sec ret"
e2e_database = "neon-app_e2e"
neon_proxy = true
"#;
        let ctx = crate::context::tests::test_ctx(toml, Some(5000));
        let vars = ctx.template_vars();
        assert_eq!(vars["services.postgres.neon_port"], "5002");
        assert_eq!(
            vars["services.postgres.neon_url"],
            "postgresql://neon-user:sec%20ret@127.0.0.1:5002/neon-app"
        );
        assert_eq!(
            vars["services.postgres.neon_e2e_url"],
            "postgresql://neon-user:sec%20ret@127.0.0.1:5002/neon-app_e2e"
        );
    }
}

