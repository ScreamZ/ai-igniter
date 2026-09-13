use crate::context::WorkspaceContext;
use anyhow::{Context, Result, bail};
use colored::Colorize;
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

/// Label set on every generated container; only containers carrying it are ever reclaimed.
pub const MANAGED_LABEL: &str = "ai-igniter.managed";

/// Local-only cluster secret for the single-node Garage instance.
const DEFAULT_GARAGE_RPC_SECRET: &str = "4425f5c26c5e11581d3223904324dcb5b5d5dfb14e5e7f35e38c595424f5f1e6";

pub struct DockerCompose<'a> {
    ctx: &'a WorkspaceContext,
    compose_file: PathBuf,
}

impl<'a> DockerCompose<'a> {
    /// Regenerates `.igniter/compose.json` from the current config, unless `compose_file` points to an external file.
    pub fn new(ctx: &'a WorkspaceContext) -> Result<Self> {
        let compose_file = match &ctx.config.compose_file {
            Some(file) => [&ctx.workspace_path, &ctx.root_path]
                .iter()
                .map(|dir| dir.join(file))
                .find(|path| path.exists())
                .with_context(|| format!("compose_file {:?} not found in workspace or root", file))?,
            None => write_generated_files(ctx)?,
        };
        Ok(Self { ctx, compose_file })
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new("docker");
        cmd.arg("compose")
            .arg("-p")
            .arg(&self.ctx.compose_project)
            .arg("-f")
            .arg(&self.compose_file)
            .current_dir(&self.ctx.workspace_path);
        // Lets external compose files reference allocated ports, e.g. "${IGNITER_POSTGRES_PORT}:5432"
        for (name, port) in &self.ctx.port_allocations {
            cmd.env(format!("IGNITER_{}_PORT", name.to_uppercase().replace('-', "_")), port.to_string());
        }
        cmd
    }

    fn run(&self, args: &[&str]) -> Result<()> {
        let action = args[0];
        let status = self
            .command()
            .args(args)
            .status()
            .with_context(|| format!("Failed to run `docker compose {action}`"))?;
        if !status.success() {
            bail!("`docker compose {action}` exited with {status}");
        }
        Ok(())
    }

    pub fn up(&self) -> Result<()> {
        println!(
            "{} Starting services for project '{}' (project: {})...",
            "[docker]".blue().bold(),
            self.ctx.config.name,
            self.ctx.compose_project.cyan()
        );
        // --remove-orphans drops containers of services disabled since the last run
        self.run(&["up", "--detach", "--wait", "--remove-orphans"])
    }

    pub fn stop(&self) -> Result<()> {
        println!(
            "{} Stopping services for project '{}'...",
            "[docker]".blue().bold(),
            self.ctx.compose_project.cyan()
        );
        self.run(&["stop"])
    }

    pub fn down(&self, remove_volumes: bool) -> Result<()> {
        println!(
            "{} Tearing down services for project '{}' (volumes: {})...",
            "[docker]".blue().bold(),
            self.ctx.compose_project.cyan(),
            if remove_volumes { "removed".red() } else { "kept".green() }
        );
        let mut args = vec!["down", "--remove-orphans"];
        if remove_volumes {
            args.push("--volumes");
        }
        self.run(&args)?;
        if remove_volumes {
            self.remove_leftover_volumes()?;
        }
        Ok(())
    }

    /// `down --volumes` only removes volumes declared in the current file, not those of services disabled since.
    fn remove_leftover_volumes(&self) -> Result<()> {
        let label = format!("label=com.docker.compose.project={}", self.ctx.compose_project);
        let output = Command::new("docker")
            .args(["volume", "ls", "-q", "--filter", &label])
            .output()
            .context("Failed to list project volumes")?;
        let listing = String::from_utf8_lossy(&output.stdout);
        let volumes: Vec<&str> = listing.split_whitespace().collect();
        if volumes.is_empty() {
            return Ok(());
        }
        let status = Command::new("docker")
            .args(["volume", "rm"])
            .args(&volumes)
            .stdout(Stdio::null())
            .status()
            .context("Failed to run `docker volume rm`")?;
        if !status.success() {
            bail!("Failed to remove leftover volumes: {}", volumes.join(", "));
        }
        Ok(())
    }

    pub fn exec(&self, service: &str, args: &[&str]) -> Result<Output> {
        self.command()
            .args(["exec", "-T", service])
            .args(args)
            .output()
            .with_context(|| format!("Failed to exec command in service {}", service))
    }

    /// Like `exec`, but fails on a non-zero exit code and returns the trimmed stdout.
    pub fn exec_checked(&self, service: &str, args: &[&str]) -> Result<String> {
        let output = self.exec(service, args)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            bail!("`{}` failed in service '{service}': {}", args.join(" "), format!("{stderr}{stdout}").trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// All containers of the project, including stopped ones.
    pub fn ps(&self) -> Result<Vec<Value>> {
        let output = self
            .command()
            .args(["ps", "--all", "--format", "json"])
            .output()
            .context("Failed to run `docker compose ps`")?;
        if !output.status.success() {
            bail!("`docker compose ps` failed: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
        Ok(parse_ps_output(&String::from_utf8_lossy(&output.stdout)))
    }

    pub fn running_services(&self) -> Result<BTreeSet<String>> {
        Ok(self
            .ps()?
            .iter()
            .filter(|c| c["State"] == "running")
            .filter_map(|c| c["Service"].as_str().map(String::from))
            .collect())
    }
}

/// `docker compose ps --format json` prints one object per line (Compose >= 2.21) or a single array (older versions).
fn parse_ps_output(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .flat_map(|value| match value {
            Value::Array(items) => items,
            other => vec![other],
        })
        .collect()
}

fn write_generated_files(ctx: &WorkspaceContext) -> Result<PathBuf> {
    let dir = ctx.igniter_dir();
    fs::create_dir_all(&dir).with_context(|| format!("Failed to create .igniter directory at {:?}", dir))?;

    if let Some(garage) = ctx.config.services.garage() {
        let path = dir.join("garage.toml");
        fs::write(&path, garage_toml(&garage.website_root_domain))
            .with_context(|| format!("Failed to write garage.toml at {:?}", path))?;
    }

    let path = ctx.compose_file_path();
    let content = serde_json::to_string_pretty(&build_compose(ctx))?;
    fs::write(&path, content).with_context(|| format!("Failed to write generated compose file at {:?}", path))?;
    Ok(path)
}

fn garage_toml(website_root_domain: &str) -> String {
    format!(
        r#"metadata_dir = "/var/lib/garage/meta"
data_dir = "/var/lib/garage/data"
db_engine = "sqlite"
replication_factor = 1
rpc_bind_addr = "[::]:3901"

[s3_api]
s3_region = "garage"
api_bind_addr = "[::]:3900"
root_domain = ".s3.garage.localhost"

[s3_web]
bind_addr = "[::]:3902"
root_domain = {}
index = "index.html"
"#,
        toml::Value::String(website_root_domain.to_string())
    )
}

/// Compose document for the enabled services. Compose accepts JSON, which avoids hand-escaping YAML.
pub fn build_compose(ctx: &WorkspaceContext) -> Value {
    let services_cfg = &ctx.config.services;
    let port = |name: &str| ctx.port_allocations[name];
    let mut services = Map::new();
    let mut volumes = Map::new();

    if let Some(pg) = services_cfg.postgres() {
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
                "ports": [format!("{}:5432", port("postgres"))],
                "volumes": ["postgres-data:/var/lib/postgresql/data"],
            }),
        );
        volumes.insert("postgres-data".into(), json!({}));
    }

    if let Some(garage) = services_cfg.garage() {
        let mut environment = json!({
            "GARAGE_DEFAULT_ACCESS_KEY": garage.access_key,
            "GARAGE_DEFAULT_SECRET_KEY": garage.secret_key,
            "GARAGE_RPC_SECRET": garage.rpc_secret.as_deref().unwrap_or(DEFAULT_GARAGE_RPC_SECRET),
        });
        let default_flag = match garage.all_buckets().first() {
            Some(bucket) => {
                environment["GARAGE_DEFAULT_BUCKET"] = json!(bucket);
                "--default-bucket"
            }
            None => "--default-access-key",
        };
        services.insert(
            "garage".into(),
            json!({
                "image": garage.image,
                "command": ["/garage", "server", "--single-node", default_flag],
                "environment": environment,
                "healthcheck": {
                    "test": ["CMD", "/garage", "status"],
                    "interval": "2s",
                    "timeout": "5s",
                    "retries": 30,
                },
                "ports": [format!("{}:3900", port("garage")), format!("{}:3902", port("garage_web"))],
                "volumes": [
                    "garage-data:/var/lib/garage/data",
                    "garage-meta:/var/lib/garage/meta",
                    format!("{}:/etc/garage.toml:ro", ctx.igniter_dir().join("garage.toml").display()),
                ],
            }),
        );
        volumes.insert("garage-data".into(), json!({}));
        volumes.insert("garage-meta".into(), json!({}));
    }

    if let Some(redis) = services_cfg.redis() {
        let mut ping = vec!["CMD", "redis-cli"];
        if let Some(password) = &redis.password {
            ping.extend(["--no-auth-warning", "-a", password]);
        }
        ping.push("ping");
        let mut service = json!({
            "image": redis.image,
            "healthcheck": { "test": ping, "interval": "2s", "timeout": "5s", "retries": 30 },
            "ports": [format!("{}:6379", port("redis"))],
            "volumes": ["redis-data:/data"],
        });
        if let Some(password) = &redis.password {
            service["command"] = json!(["redis-server", "--requirepass", password]);
        }
        services.insert("redis".into(), service);
        volumes.insert("redis-data".into(), json!({}));
    }

    for (name, custom) in &services_cfg.custom {
        let mut service = json!({ "image": custom.image });
        if let (Some(host_port), Some(target)) = (ctx.port_allocations.get(name), custom.target_port) {
            service["ports"] = json!([format!("{host_port}:{target}")]);
        }
        if !custom.environment.is_empty() {
            service["environment"] = json!(custom.environment);
        }
        if !custom.command.is_empty() {
            service["command"] = json!(custom.command);
        }
        if !custom.volumes.is_empty() {
            let specs: Vec<String> = custom
                .volumes
                .iter()
                .map(|spec| resolve_volume(ctx, spec, &mut volumes))
                .collect();
            service["volumes"] = json!(specs);
        }
        services.insert(name.clone(), service);
    }

    for service in services.values_mut() {
        service["labels"] = json!({ MANAGED_LABEL: "true" });
    }

    let mut compose = json!({ "services": services });
    if !volumes.is_empty() {
        compose["volumes"] = Value::Object(volumes);
    }
    escape_interpolation(&mut compose);
    compose
}

/// Relative bind mounts are resolved against the workspace (not `.igniter/`); named volumes get declared.
fn resolve_volume(ctx: &WorkspaceContext, spec: &str, named_volumes: &mut Map<String, Value>) -> String {
    let Some((source, target)) = spec.split_once(':') else {
        return spec.to_string(); // anonymous volume
    };
    if source.starts_with('.') {
        return format!("{}:{target}", ctx.workspace_path.join(source).display());
    }
    if !source.contains('/') && !source.starts_with('~') {
        named_volumes.insert(source.to_string(), json!({}));
    }
    spec.to_string()
}

/// Compose interpolates `$` in values; generated values are always literal.
fn escape_interpolation(value: &mut Value) {
    match value {
        Value::String(s) if s.contains('$') => *s = s.replace('$', "$$"),
        Value::Array(items) => items.iter_mut().for_each(escape_interpolation),
        Value::Object(map) => map.values_mut().for_each(escape_interpolation),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::tests::test_ctx;

    const CONFIG: &str = r#"
name = "app"
[services.postgres]
database = "app"
user = "app"
password = "pa$$word"
[services.garage]
access_key = "k"
secret_key = "s"
[services.redis]
enabled = false
[services.custom.mail]
image = "axllent/mailpit"
port_offset = 8
target_port = 8025
environment = { MP_UI_BIND = "0.0.0.0:8025" }
command = ["--smtp-auth-accept-any"]
volumes = ["./data/mail:/data", "mail-cache:/cache", "/abs:/abs"]
"#;

    #[test]
    fn generates_enabled_services_only_with_managed_labels() {
        let compose = build_compose(&test_ctx(CONFIG, Some(4000)));
        let names: Vec<&String> = compose["services"].as_object().unwrap().keys().collect();
        assert_eq!(names, ["garage", "mail", "postgres"]);
        for service in compose["services"].as_object().unwrap().values() {
            assert_eq!(service["labels"][MANAGED_LABEL], "true");
        }
    }

    #[test]
    fn escapes_dollar_signs_and_uses_tcp_healthcheck() {
        let compose = build_compose(&test_ctx(CONFIG, Some(4000)));
        let pg = &compose["services"]["postgres"];
        assert_eq!(pg["environment"]["POSTGRES_PASSWORD"], "pa$$$$word");
        assert_eq!(pg["healthcheck"]["test"][2], "-h");
        assert_eq!(pg["ports"][0], "4001:5432");
    }

    #[test]
    fn garage_without_buckets_only_creates_the_key() {
        let config = "name = \"app\"\n[services.garage]\naccess_key = \"k\"\nsecret_key = \"s\"";
        let compose = build_compose(&test_ctx(config, Some(4000)));
        assert_eq!(compose["services"]["garage"]["command"][3], "--default-access-key");
        assert!(compose["services"]["garage"]["environment"]["GARAGE_DEFAULT_BUCKET"].is_null());
    }

    #[test]
    fn wires_custom_service_fields() {
        let compose = build_compose(&test_ctx(CONFIG, Some(4000)));
        let mail = &compose["services"]["mail"];
        assert_eq!(mail["ports"][0], "4008:8025");
        assert_eq!(mail["environment"]["MP_UI_BIND"], "0.0.0.0:8025");
        assert_eq!(mail["command"][0], "--smtp-auth-accept-any");
        assert_eq!(mail["volumes"][0], "/work/Feature X/./data/mail:/data");
        assert_eq!(mail["volumes"][2], "/abs:/abs");
        assert!(compose["volumes"]["mail-cache"].is_object());
    }

    #[test]
    fn parses_ndjson_and_array_ps_output() {
        let ndjson = "{\"Service\":\"a\",\"State\":\"running\"}\n{\"Service\":\"b\",\"State\":\"exited\"}\n";
        assert_eq!(parse_ps_output(ndjson).len(), 2);
        assert_eq!(parse_ps_output("[{\"Service\":\"a\"}]").len(), 1);
        assert!(parse_ps_output("").is_empty());
    }
}
