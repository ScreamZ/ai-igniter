use super::{Service, run_shell};
use crate::config::PostgresConfig;
use crate::context::WorkspaceContext;
use crate::docker::DockerCompose;
use crate::env_writer::EnvWriter;
use anyhow::{Context, Result};
use colored::Colorize;
use std::collections::BTreeMap;

/// Stored as the database comment once seeded; it disappears with the volume, so fresh databases get seeded again.
const SEED_MARKER: &str = "ai-igniter:seeded";

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
mod tests {
    use super::*;

    #[test]
    fn quotes_sql_identifiers_and_literals() {
        assert_eq!(quote_ident("ai-tools_e2e"), "\"ai-tools_e2e\"");
        assert_eq!(quote_ident("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(quote_literal("o'brien"), "'o''brien'");
    }
}
