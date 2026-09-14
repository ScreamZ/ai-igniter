use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::services::all_reserved_names;
pub use crate::services::garage::GarageConfig;
#[allow(unused_imports)]
pub use crate::services::postgres::NeonProxyConfig;
pub use crate::services::postgres::PostgresConfig;
use crate::services::BUILTIN_SERVICES;

pub const CONFIG_FILE_NAME: &str = "ai-igniter.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub name: String,
    /// Fixed base port. When unset and no orchestrator port is provided, it is derived from the workspace path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_port: Option<u16>,

    /// Existing compose file used instead of the generated one (relative to the workspace, then the root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compose_file: Option<PathBuf>,

    /// Command to execute during dev mode after services are healthy (e.g. "bun run dev").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dev_command: Option<String>,

    #[serde(default)]
    pub orchestrator: OrchestratorConfig,

    #[serde(default)]
    pub services: ServicesConfig,

    #[serde(default)]
    pub env_template: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OrchestratorConfig {
    /// Env var holding the workspace base port (e.g. PASEO_PORT)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_env: Option<String>,
    /// Env var holding the source checkout path, used to seed `.env` in new worktrees
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_env: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServicesConfig {
    pub postgres: Option<PostgresConfig>,
    pub garage: Option<GarageConfig>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom: BTreeMap<String, CustomServiceConfig>,
}

impl ServicesConfig {
    #[allow(dead_code)]
    pub fn postgres(&self) -> Option<&PostgresConfig> {
        self.postgres.as_ref().filter(|c| c.enabled)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn garage(&self) -> Option<&GarageConfig> {
        self.garage.as_ref().filter(|c| c.enabled)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomServiceConfig {
    pub image: String,
    pub port_offset: Option<u16>,
    pub target_port: Option<u16>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub volumes: Vec<String>,
}

impl Config {
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file at {:?}", path))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("Failed to parse TOML in {:?}", path))?;
        Ok(config)
    }

    pub fn find_config(workspace_path: &Path, root_path: &Path) -> Result<(PathBuf, Self)> {
        let candidates = [
            workspace_path.join(CONFIG_FILE_NAME),
            workspace_path.join(format!(".{}", CONFIG_FILE_NAME)),
            root_path.join(CONFIG_FILE_NAME),
            root_path.join(format!(".{}", CONFIG_FILE_NAME)),
        ];

        for candidate in &candidates {
            if candidate.exists() {
                let cfg = Self::load_from_file(candidate)?;
                return Ok((candidate.clone(), cfg));
            }
        }

        anyhow::bail!(
            "Configuration file '{}' not found in workspace ({:?}) or root ({:?}). Run `ai-igniter init` to create one.",
            CONFIG_FILE_NAME,
            workspace_path,
            root_path
        )
    }

    /// Host port offsets of enabled services, keyed by allocation name.
    pub fn port_offsets(&self) -> Vec<(String, u16)> {
        let mut offsets = Vec::new();
        for provider in BUILTIN_SERVICES {
            offsets.extend(provider.port_offsets(&self.services));
        }
        for (name, custom) in &self.services.custom {
            if let Some(offset) = custom.port_offset {
                offsets.push((name.clone(), offset));
            }
        }
        offsets
    }

    pub fn validate(&self) -> Result<()> {
        if sanitize_name(&self.name).is_empty() {
            bail!("`name` must contain at least one ASCII letter or digit");
        }

        let reserved = all_reserved_names();
        for (name, custom) in &self.services.custom {
            let valid = name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
            if !valid {
                bail!("Custom service name '{name}' must match [a-z0-9][a-z0-9_-]*");
            }
            if reserved.contains(&name.as_str()) {
                bail!("Custom service name '{name}' is reserved");
            }
            if custom.port_offset.is_some() != custom.target_port.is_some() {
                bail!("Custom service '{name}': `port_offset` and `target_port` must be set together");
            }
        }

        let offsets = self.port_offsets();
        let mut seen: HashMap<u16, &str> = HashMap::new();
        for (name, offset) in &offsets {
            if *offset == 0 {
                bail!("Service '{name}' has port offset 0, which collides with the base (app) port");
            }
            if let Some(other) = seen.insert(*offset, name) {
                bail!("Services '{other}' and '{name}' share port offset {offset}");
            }
        }
        Ok(())
    }
}

/// Lowercases and replaces characters Docker Compose rejects in project names.
pub fn sanitize_name(raw: &str) -> String {
    raw.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .collect::<String>()
        .trim_matches(|c| c == '-' || c == '_')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_str: &str) -> Config {
        toml::from_str(toml_str).unwrap()
    }

    #[test]
    fn base_port_is_optional() {
        assert_eq!(parse("name = \"my-project\"").base_port, None);
        assert_eq!(parse("name = \"my-project\"\nbase_port = 4000").base_port, Some(4000));
    }

    #[test]
    fn base_port_serialized_only_when_set() {
        let mut config = parse("name = \"my-project\"");
        assert!(!toml::to_string_pretty(&config).unwrap().contains("base_port"));
        config.base_port = Some(4500);
        assert!(toml::to_string_pretty(&config).unwrap().contains("base_port = 4500"));
    }

    #[test]
    fn dev_command_is_optional_and_serializes_when_set() {
        let config = parse("name = \"my-project\"");
        assert_eq!(config.dev_command, None);
        assert!(!toml::to_string_pretty(&config).unwrap().contains("dev_command"));

        let with_cmd = parse("name = \"my-project\"\ndev_command = \"bun run dev\"");
        assert_eq!(with_cmd.dev_command.as_deref(), Some("bun run dev"));
        assert!(toml::to_string_pretty(&with_cmd).unwrap().contains("dev_command = \"bun run dev\""));
    }

    #[test]
    fn legacy_workspace_env_is_ignored() {
        let config = parse("name = \"p\"\n[orchestrator]\nport_env = \"PASEO_PORT\"\nworkspace_env = \"PASEO_WORKTREE_PATH\"");
        assert_eq!(config.orchestrator.port_env.as_deref(), Some("PASEO_PORT"));
    }

    #[test]
    fn rejects_duplicate_offsets() {
        let config = parse(
            r#"
name = "p"
[services.postgres]
database = "p"
user = "p"
password = "p"
[services.garage]
access_key = "k"
secret_key = "s"
port_offset = 1
"#,
        );
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("share port offset 1"), "{err}");
    }

    #[test]
    fn disabled_services_do_not_reserve_offsets() {
        let config = parse(
            r#"
name = "p"
[services.garage]
enabled = false
access_key = "k"
secret_key = "s"
port_offset = 1
[services.custom.mail]
image = "axllent/mailpit"
port_offset = 1
target_port = 8025
"#,
        );
        config.validate().unwrap();
    }

    #[test]
    fn rejects_zero_offset_and_reserved_custom_names() {
        let zero = parse("name = \"p\"\n[services.postgres]\ndatabase = \"p\"\nuser = \"p\"\npassword = \"p\"\nport_offset = 0");
        assert!(zero.validate().is_err());
        let reserved = parse("name = \"p\"\n[services.custom.postgres]\nimage = \"x\"");
        assert!(reserved.validate().is_err());
        let half_port = parse("name = \"p\"\n[services.custom.mail]\nimage = \"x\"\nport_offset = 7");
        assert!(half_port.validate().is_err());
    }

    #[test]
    fn sanitizes_names_for_compose() {
        assert_eq!(sanitize_name("My App"), "my-app");
        assert_eq!(sanitize_name("_ai.tools-"), "ai-tools");
        assert_eq!(sanitize_name("éé"), "");
    }

    #[test]
    fn all_buckets_dedupes_website_buckets() {
        let config = parse(
            r#"
name = "p"
[services.garage]
access_key = "k"
secret_key = "s"
buckets = ["assets", "uploads"]
website_buckets = ["assets", "site"]
"#,
        );
        assert_eq!(config.services.garage().unwrap().all_buckets(), ["assets", "uploads", "site"]);
    }

    #[test]
    fn postgres_neon_proxy_reserves_offset_and_names() {
        let config = parse(
            r#"
name = "p"
[services.postgres]
database = "p"
user = "u"
password = "p"
neon_proxy = true
[services.custom.app2]
image = "myimage"
port_offset = 2
target_port = 8080
"#,
        );
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("share port offset 2"), "{err}");

        let reserved_name = parse(
            r#"
name = "p"
[services.custom.neon-proxy]
image = "myimage"
"#,
        );
        assert!(reserved_name.validate().is_err());
    }
}
