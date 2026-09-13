use crate::cli::Cli;
use crate::config::{CONFIG_FILE_NAME, Config, sanitize_name};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Orchestrator variables pointing at the worktree, only consulted when invoked outside any project.
const WORKSPACE_ENV_VARS: &[&str] = &[
    "WORKSPACE_PATH",
    "PASEO_WORKTREE_PATH",
    "CONDUCTOR_WORKSPACE_PATH",
    "ORCA_WORKSPACE_PATH",
];

/// Generic base port variables, consulted after `orchestrator.port_env`.
const PORT_ENV_VARS: &[&str] = &["WORKSPACE_PORT", "PASEO_PORT", "CONDUCTOR_PORT"];

#[derive(Debug, Clone)]
pub struct WorkspaceContext {
    pub workspace_path: PathBuf,
    pub root_path: PathBuf,
    pub slug: String,
    pub hash: String,
    pub compose_project: String,
    pub base_port: u16,
    pub is_local: bool,
    pub config_path: PathBuf,
    pub config: Config,
    pub port_allocations: BTreeMap<String, u16>,
}

impl WorkspaceContext {
    pub fn resolve(cli: &Cli) -> Result<Self> {
        let current_dir = std::env::current_dir().context("Failed to get current directory")?;

        // 1. Workspace: --dir, else the enclosing project (never hijacked by external env), else orchestrator env
        let requested = cli.dir.clone().unwrap_or_else(|| locate_workspace(&current_dir));
        let workspace_path = std::fs::canonicalize(&requested)
            .with_context(|| format!("Workspace directory {:?} does not exist", requested))?;
        let main_checkout = git_main_checkout(&workspace_path);

        // 2. Config: workspace first, then the main checkout (worktrees may not carry an untracked config)
        let (config_path, config) = match &cli.config {
            Some(path) => (path.clone(), Config::load_from_file(path)?),
            None => Config::find_config(&workspace_path, main_checkout.as_deref().unwrap_or(&workspace_path))?,
        };

        // 3. Root: --root, else $<orchestrator.root_env>, else the main git checkout
        let root_path = cli
            .root
            .clone()
            .or_else(|| env_dir(config.orchestrator.root_env.as_deref()?))
            .or(main_checkout)
            .unwrap_or_else(|| workspace_path.clone());

        let base_port = resolve_base_port(cli.port, &config)?;
        Self::from_parts(workspace_path, root_path, config_path, config, base_port, is_local())
    }

    /// Builds the context from resolved paths; a missing `base_port` is derived from the workspace path.
    pub fn from_parts(
        workspace_path: PathBuf,
        root_path: PathBuf,
        config_path: PathBuf,
        config: Config,
        base_port: Option<u16>,
        is_local: bool,
    ) -> Result<Self> {
        config
            .validate()
            .with_context(|| format!("Invalid configuration in {:?}", config_path))?;

        let digest = Sha256::digest(workspace_path.to_string_lossy().as_bytes());
        let hash = hex::encode(&digest[..4]);
        let slug = workspace_path
            .file_name()
            .map(|n| sanitize_name(&n.to_string_lossy()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "workspace".to_string());
        let compose_project = format!("{}-{}-{}", sanitize_name(&config.name), slug, hash);

        let base_port = base_port.unwrap_or_else(|| derived_base_port(&digest));
        let port_allocations = allocate_ports(&config, base_port)?;

        Ok(Self {
            workspace_path,
            root_path,
            slug,
            hash,
            compose_project,
            base_port,
            is_local,
            config_path,
            config,
            port_allocations,
        })
    }

    pub fn template_vars(&self) -> BTreeMap<String, String> {
        let mut vars = BTreeMap::new();

        vars.insert("project.name".into(), self.config.name.clone());
        vars.insert("workspace.slug".into(), self.slug.clone());
        vars.insert("workspace.hash".into(), self.hash.clone());
        vars.insert("workspace.compose_project".into(), self.compose_project.clone());
        vars.insert("workspace.path".into(), self.workspace_path.display().to_string());
        vars.insert("workspace.root".into(), self.root_path.display().to_string());

        for (name, port) in &self.port_allocations {
            vars.insert(format!("ports.{name}"), port.to_string());
        }

        for provider in crate::services::BUILTIN_SERVICES {
            provider.contribute_template_vars(self, &mut vars);
        }

        for name in self.config.services.custom.keys() {
            if let Some(port) = self.port_allocations.get(name) {
                vars.insert(format!("services.{name}.port"), port.to_string());
            }
        }

        vars
    }

    pub fn interpolate(&self, text: &str) -> Result<String> {
        render_template(text, &self.template_vars()).map_err(|unknown| {
            anyhow::anyhow!("Unknown template placeholder(s) {} in {:?}", format_placeholders(&unknown), text)
        })
    }

    pub fn igniter_dir(&self) -> PathBuf {
        self.workspace_path.join(".igniter")
    }

    pub fn compose_file_path(&self) -> PathBuf {
        self.igniter_dir().join("compose.json")
    }

    /// Image used by each configured and enabled service.
    pub fn service_images(&self) -> BTreeMap<String, String> {
        let mut images = BTreeMap::new();
        if let Some(pg) = &self.config.services.postgres {
            if pg.enabled {
                images.insert("postgres".to_string(), pg.image.clone());
            }
        }
        if let Some(garage) = &self.config.services.garage {
            if garage.enabled {
                images.insert("garage".to_string(), garage.image.clone());
            }
        }
        for (name, custom) in &self.config.services.custom {
            images.insert(name.clone(), custom.image.clone());
        }
        images
    }
}

/// Replaces `{{var}}` and `{{var + N}}` placeholders; returns the unknown expressions on failure.
pub fn render_template(text: &str, vars: &BTreeMap<String, String>) -> Result<String, Vec<String>> {
    let mut out = String::with_capacity(text.len());
    let mut unknown = Vec::new();
    let mut rest = text;

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let expr = after[..end].trim();
        match eval_expr(expr, vars) {
            Some(value) => out.push_str(&value),
            None => unknown.push(expr.to_string()),
        }
        rest = &after[end + 2..];
    }
    // Either no placeholder left, or an unterminated `{{` kept verbatim
    if let Some(start) = rest.find("{{") {
        out.push_str(&rest[start..]);
    } else {
        out.push_str(rest);
    }

    if unknown.is_empty() { Ok(out) } else { Err(unknown) }
}

pub fn format_placeholders(unknown: &[String]) -> String {
    unknown.iter().map(|u| format!("{{{{{u}}}}}")).collect::<Vec<_>>().join(", ")
}

fn eval_expr(expr: &str, vars: &BTreeMap<String, String>) -> Option<String> {
    if let Some((lhs, rhs)) = expr.split_once('+') {
        let base: u32 = vars.get(lhs.trim())?.parse().ok()?;
        let offset: u32 = rhs.trim().parse().ok()?;
        return u16::try_from(base + offset).ok().map(|p| p.to_string());
    }
    vars.get(expr).cloned()
}

/// Percent-encodes everything but RFC 3986 unreserved characters (for URL userinfo and paths).
pub(crate) fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// 2000 slots of 20 ports in 20000..=59999, so worktrees without an orchestrator port don't collide.
fn derived_base_port(digest: &[u8]) -> u16 {
    let slot = u16::from_be_bytes([digest[4], digest[5]]) % 2000;
    20000 + slot * 20
}

fn allocate_ports(config: &Config, base_port: u16) -> Result<BTreeMap<String, u16>> {
    let mut ports = BTreeMap::from([("base".to_string(), base_port)]);
    for (name, offset) in config.port_offsets() {
        let port = base_port.checked_add(offset).with_context(|| {
            format!("Port for '{name}' is out of range: base port {base_port} + offset {offset} > 65535")
        })?;
        ports.insert(name, port);
    }
    Ok(ports)
}

fn resolve_base_port(cli_port: Option<u16>, config: &Config) -> Result<Option<u16>> {
    if cli_port.is_some() {
        return Ok(cli_port);
    }
    let env_names = config.orchestrator.port_env.iter().map(String::as_str).chain(PORT_ENV_VARS.iter().copied());
    for name in env_names {
        if let Ok(value) = std::env::var(name) {
            let port = value
                .trim()
                .parse::<u16>()
                .with_context(|| format!("${name}={value:?} is not a valid port"))?;
            return Ok(Some(port));
        }
    }
    Ok(config.base_port)
}

fn locate_workspace(current_dir: &Path) -> PathBuf {
    let toplevel = git_toplevel(current_dir);
    // Stop at the worktree boundary: a worktree nested in the main checkout must not adopt its project
    if let Some(dir) = find_nearest_config_dir(current_dir, toplevel.as_deref()) {
        return dir;
    }
    // Inside a git worktree without its own config: the config may live in the main checkout
    if let Some(toplevel) = toplevel {
        return toplevel;
    }
    WORKSPACE_ENV_VARS
        .iter()
        .find_map(|name| env_dir(name))
        .unwrap_or_else(|| current_dir.to_path_buf())
}

fn find_nearest_config_dir(start: &Path, boundary: Option<&Path>) -> Option<PathBuf> {
    let boundary = boundary.map(|b| std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf()));
    let mut current = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    loop {
        if current.join(CONFIG_FILE_NAME).exists() || current.join(format!(".{CONFIG_FILE_NAME}")).exists() {
            return Some(current);
        }
        if boundary.as_deref() == Some(current.as_path()) || !current.pop() {
            return None;
        }
    }
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from).filter(|p| p.is_dir())
}

fn git(path: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").arg("-C").arg(path).args(args).output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !stdout.is_empty()).then_some(stdout)
}

fn git_toplevel(path: &Path) -> Option<PathBuf> {
    git(path, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

/// Main checkout of the repository, also when `path` is a linked worktree.
fn git_main_checkout(path: &Path) -> Option<PathBuf> {
    let common_dir = PathBuf::from(git(path, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?);
    if !common_dir.ends_with(".git") {
        return None; // bare repository
    }
    common_dir.parent().map(Path::to_path_buf)
}

fn is_local() -> bool {
    std::env::var("IS_LOCAL")
        .or_else(|_| std::env::var("PASEO_IS_LOCAL"))
        .or_else(|_| std::env::var("CONDUCTOR_IS_LOCAL"))
        .map(|val| val != "0" && val.to_lowercase() != "false")
        .unwrap_or(true)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn test_ctx(config_toml: &str, base_port: Option<u16>) -> WorkspaceContext {
        let config: Config = toml::from_str(config_toml).unwrap();
        WorkspaceContext::from_parts(
            PathBuf::from("/work/Feature X"),
            PathBuf::from("/work/main"),
            PathBuf::from("/work/main/ai-igniter.toml"),
            config,
            base_port,
            true,
        )
        .unwrap()
    }

    const FULL: &str = r#"
name = "My App"
[services.postgres]
database = "my-app"
user = "my-app"
password = "p@ss word"
e2e_database = "my-app_e2e"
[services.garage]
access_key = "k"
secret_key = "s"
[services.custom.mail]
image = "axllent/mailpit"
port_offset = 8
target_port = 8025
"#;

    #[test]
    fn allocates_ports_from_offsets() {
        let ctx = test_ctx(FULL, Some(4000));
        let expected = [("base", 4000), ("garage", 4003), ("garage_web", 4004), ("mail", 4008), ("postgres", 4001)];
        let actual: Vec<(&str, u16)> = ctx.port_allocations.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn sanitizes_compose_project() {
        let ctx = test_ctx(FULL, Some(4000));
        assert_eq!(ctx.compose_project, format!("my-app-feature-x-{}", ctx.hash));
    }

    #[test]
    fn derives_stable_base_port_when_unset() {
        let a = test_ctx(FULL, None);
        let b = test_ctx(FULL, None);
        assert_eq!(a.base_port, b.base_port);
        assert!((20000..60000).contains(&a.base_port) && a.base_port.is_multiple_of(20));
    }

    #[test]
    fn rejects_port_overflow() {
        let config: Config = toml::from_str(FULL).unwrap();
        let err = WorkspaceContext::from_parts("/w".into(), "/w".into(), "/w/c".into(), config, Some(65534), true);
        assert!(err.is_err());
    }

    #[test]
    fn exposes_service_vars_with_encoded_credentials() {
        let vars = test_ctx(FULL, Some(4000)).template_vars();
        assert_eq!(vars["services.postgres.url"], "postgresql://my-app:p%40ss%20word@127.0.0.1:4001/my-app");
        assert_eq!(vars["services.postgres.e2e_url"], "postgresql://my-app:p%40ss%20word@127.0.0.1:4001/my-app_e2e");
        assert_eq!(vars["services.garage.web_port"], "4004");
        assert_eq!(vars["services.mail.port"], "4008");
    }

    #[test]
    fn renders_placeholders_and_arithmetic() {
        let vars = test_ctx(FULL, Some(4000)).template_vars();
        let rendered = render_template("http://localhost:{{ ports.base + 10 }}/{{project.name}}", &vars).unwrap();
        assert_eq!(rendered, "http://localhost:4010/My App");
        assert_eq!(render_template("a {{ unterminated", &vars).unwrap(), "a {{ unterminated");
    }

    #[test]
    fn reports_unknown_placeholders() {
        let ctx = test_ctx("name = \"p\"", Some(4000));
        let err = render_template("{{services.unknown.port}}-{{ports.base + 70000}}", &ctx.template_vars()).unwrap_err();
        assert_eq!(err, ["services.unknown.port", "ports.base + 70000"]);
    }
}
