use crate::config::OrchestratorConfig;

/// Specification and environment variables for an external AI orchestrator or workspace tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrchestratorSpec {
    /// Human-readable name (e.g. "Paseo", "Conductor", "Orca").
    pub name: &'static str,
    /// Environment variable pointing at the worktree/workspace path.
    pub workspace_env: &'static str,
    /// Environment variable pointing at the workspace base port.
    pub port_env: &'static str,
    /// Environment variable pointing at the main source checkout path (if applicable).
    pub root_env: Option<&'static str>,
    /// Environment variable indicating whether execution is local vs in a cloud sandbox.
    pub is_local_env: Option<&'static str>,
}

impl OrchestratorSpec {
    /// Converts this specification into an `OrchestratorConfig` as saved in `igniter.toml`.
    pub fn to_orchestrator_config(self) -> OrchestratorConfig {
        OrchestratorConfig {
            port_env: Some(self.port_env.to_string()),
            root_env: self.root_env.map(|s| s.to_string()),
        }
    }
}

/// Known orchestrators supported out of the box with dedicated environment variable mappings.
pub const KNOWN_ORCHESTRATORS: &[OrchestratorSpec] = &[
    OrchestratorSpec {
        name: "Paseo",
        workspace_env: "PASEO_WORKTREE_PATH",
        port_env: "PASEO_PORT",
        root_env: Some("PASEO_SOURCE_CHECKOUT_PATH"),
        is_local_env: Some("PASEO_IS_LOCAL"),
    },
    OrchestratorSpec {
        name: "Conductor",
        workspace_env: "CONDUCTOR_WORKSPACE_PATH",
        port_env: "CONDUCTOR_PORT",
        root_env: Some("CONDUCTOR_ROOT_PATH"),
        is_local_env: Some("CONDUCTOR_IS_LOCAL"),
    },
    OrchestratorSpec {
        name: "Orca",
        workspace_env: "ORCA_WORKSPACE_PATH",
        port_env: "ORCA_PORT",
        root_env: Some("ORCA_ROOT_PATH"),
        is_local_env: None,
    },
];

pub const GENERIC_WORKSPACE_ENV: &str = "WORKSPACE_PATH";
pub const GENERIC_PORT_ENV: &str = "WORKSPACE_PORT";
pub const GENERIC_ROOT_ENV: &str = "WORKSPACE_ROOT_PATH";
pub const GENERIC_IS_LOCAL_ENV: &str = "IS_LOCAL";

/// Returns all candidate environment variable names for locating the workspace directory,
/// starting with the generic fallback `WORKSPACE_PATH`, followed by each known orchestrator.
pub fn workspace_env_vars<'a>() -> impl Iterator<Item = &'a str> {
    std::iter::once(GENERIC_WORKSPACE_ENV)
        .chain(KNOWN_ORCHESTRATORS.iter().map(|s| s.workspace_env))
}

/// Returns all candidate generic and known orchestrator environment variable names for base port fallback.
pub fn port_env_vars<'a>() -> impl Iterator<Item = &'a str> {
    std::iter::once(GENERIC_PORT_ENV).chain(KNOWN_ORCHESTRATORS.iter().map(|s| s.port_env))
}

/// Returns true if execution is determined to be local (defaults to true if unset).
/// Checked against `IS_LOCAL` first, then each orchestrator's specific `is_local_env`.
pub fn check_is_local() -> bool {
    check_is_local_from(|key| std::env::var(key).ok())
}

/// Helper allowing environment lookup dependency injection for testability.
pub fn check_is_local_from<F>(lookup: F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(val) = lookup(GENERIC_IS_LOCAL_ENV) {
        return val != "0" && val.to_lowercase() != "false";
    }
    for spec in KNOWN_ORCHESTRATORS {
        if let Some(val) = spec.is_local_env.and_then(&lookup) {
            return val != "0" && val.to_lowercase() != "false";
        }
    }
    true
}

/// Finds an orchestrator specification by name (case-insensitive).
pub fn find_by_name(name: &str) -> Option<&'static OrchestratorSpec> {
    KNOWN_ORCHESTRATORS
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_known_orchestrators_lookups() {
        let paseo = find_by_name("Paseo").expect("Paseo should be found");
        assert_eq!(paseo.workspace_env, "PASEO_WORKTREE_PATH");
        assert_eq!(paseo.port_env, "PASEO_PORT");
        assert_eq!(paseo.root_env, Some("PASEO_SOURCE_CHECKOUT_PATH"));
        assert_eq!(paseo.is_local_env, Some("PASEO_IS_LOCAL"));

        let cfg = paseo.to_orchestrator_config();
        assert_eq!(cfg.port_env.as_deref(), Some("PASEO_PORT"));
        assert_eq!(cfg.root_env.as_deref(), Some("PASEO_SOURCE_CHECKOUT_PATH"));

        assert!(find_by_name("conductor").is_some());
        assert!(find_by_name("ORCA").is_some());
        assert!(find_by_name("nonexistent").is_none());
    }

    #[test]
    fn test_workspace_and_port_env_lists() {
        let ws_vars: Vec<&str> = workspace_env_vars().collect();
        assert_eq!(ws_vars[0], GENERIC_WORKSPACE_ENV);
        assert!(ws_vars.contains(&"PASEO_WORKTREE_PATH"));
        assert!(ws_vars.contains(&"CONDUCTOR_WORKSPACE_PATH"));
        assert!(ws_vars.contains(&"ORCA_WORKSPACE_PATH"));

        let port_vars: Vec<&str> = port_env_vars().collect();
        assert_eq!(port_vars[0], GENERIC_PORT_ENV);
        assert!(port_vars.contains(&"PASEO_PORT"));
        assert!(port_vars.contains(&"CONDUCTOR_PORT"));
        assert!(port_vars.contains(&"ORCA_PORT"));
    }

    #[test]
    fn test_check_is_local() {
        let empty_map = HashMap::<&str, String>::new();
        assert!(check_is_local_from(|k| empty_map.get(k).cloned()));

        let mut map = HashMap::new();
        map.insert("IS_LOCAL", "0".to_string());
        assert!(!check_is_local_from(|k| map.get(k).cloned()));

        let mut map2 = HashMap::new();
        map2.insert("PASEO_IS_LOCAL", "false".to_string());
        assert!(!check_is_local_from(|k| map2.get(k).cloned()));

        let mut map3 = HashMap::new();
        map3.insert("CONDUCTOR_IS_LOCAL", "1".to_string());
        assert!(check_is_local_from(|k| map3.get(k).cloned()));
    }
}
