use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::net::TcpListener;

use crate::config::Config;

use std::sync::Mutex;

static PORT_CHECK_LOCK: Mutex<()> = Mutex::new(());

/// Checks if a TCP port is available for binding on localhost.
pub fn is_port_available(port: u16) -> bool {
    let _guard = PORT_CHECK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Finds the first available TCP port starting at `start_port` that is not in `exclude`.
/// Scans up to 65535, then wraps around to 10000..start_port if needed.
pub fn find_available_port<F>(start_port: u16, exclude: &[u16], check: F) -> Option<u16>
where
    F: Fn(u16) -> bool,
{
    (start_port..=u16::MAX)
        .find(|&port| !exclude.contains(&port) && check(port))
        .or_else(|| (10000..start_port).find(|&port| !exclude.contains(&port) && check(port)))
}

/// 2000 slots of 20 ports in 20000..=59999, so different worktrees receive stable non-colliding base ports.
pub fn derived_base_port(digest: &[u8]) -> u16 {
    let slot = u16::from_be_bytes([digest[4], digest[5]]) % 2000;
    20000 + slot * 20
}

/// Resolves the base port with the given availability checker.
/// Priority:
/// 1. CLI `--port` (honored directly; warns if port is already in use)
/// 2. Configured `base_port` (falls back to next available port if in use)
/// 3. Deterministic hash-derived port (falls back to next available port if in use)
pub fn resolve_base_port_with_checker<F>(
    cli_port: Option<u16>,
    config_base_port: Option<u16>,
    digest: &[u8],
    check: F,
) -> Result<(u16, Option<String>)>
where
    F: Fn(u16) -> bool,
{
    if let Some(port) = cli_port {
        let warning = if !check(port) {
            Some(format!(
                "Port {port} specified via --port is already in use on the host."
            ))
        } else {
            None
        };
        return Ok((port, warning));
    }

    if let Some(configured) = config_base_port {
        if check(configured) {
            return Ok((configured, None));
        }
        let chosen = find_available_port(configured + 1, &[], &check)
            .context("No available TCP port found on the system")?;
        let warning = Some(format!(
            "Configured base_port {configured} is already in use; fell back to available port {chosen}."
        ));
        return Ok((chosen, warning));
    }

    let preferred = derived_base_port(digest);
    if check(preferred) {
        return Ok((preferred, None));
    }

    let chosen = find_available_port(preferred + 1, &[], &check)
        .context("No available TCP port found on the system")?;
    let warning = Some(format!(
        "Derived base port {preferred} is already in use; fell back to available port {chosen}."
    ));
    Ok((chosen, warning))
}

/// Resolves the base port using real TCP socket availability checks.
pub fn resolve_base_port(
    cli_port: Option<u16>,
    config_base_port: Option<u16>,
    digest: &[u8],
) -> Result<(u16, Option<String>)> {
    resolve_base_port_with_checker(cli_port, config_base_port, digest, is_port_available)
}

/// Allocates ports for the base application and all configured services.
/// Each service prefers `base_port + offset`, but if that specific port is already in use,
/// an independent available port is automatically allocated so there are zero conflicts.
pub fn allocate_ports_with_checker<F>(
    config: &Config,
    base_port: u16,
    check: F,
) -> Result<(BTreeMap<String, u16>, Vec<String>)>
where
    F: Fn(u16) -> bool,
{
    let mut ports = BTreeMap::from([("base".to_string(), base_port)]);
    let mut allocated = vec![base_port];
    let mut warnings = Vec::new();

    for (name, offset) in config.port_offsets() {
        let candidate = base_port.checked_add(offset);
        let port = match candidate {
            Some(cand) if !allocated.contains(&cand) && check(cand) => cand,
            Some(cand) => {
                let fallback = find_available_port(cand + 1, &allocated, &check).with_context(|| {
                    format!("Failed to find an available port for service '{name}'")
                })?;
                warnings.push(format!(
                    "Port {cand} for service '{name}' is in use; dynamically allocated port {fallback}."
                ));
                fallback
            }
            None => {
                let fallback = find_available_port(10000, &allocated, &check).with_context(|| {
                    format!("Port offset {offset} out of range for service '{name}' and no fallback port found")
                })?;
                warnings.push(format!(
                    "Port offset {offset} for service '{name}' is out of range; dynamically allocated port {fallback}."
                ));
                fallback
            }
        };

        allocated.push(port);
        ports.insert(name, port);
    }

    Ok((ports, warnings))
}

/// Allocates ports for the base application and all configured services using real TCP checks.
pub fn allocate_ports(config: &Config, base_port: u16) -> Result<(BTreeMap<String, u16>, Vec<String>)> {
    allocate_ports_with_checker(config, base_port, is_port_available)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn test_derived_base_port_deterministic() {
        let digest1 = Sha256::digest(b"/path/to/my-repo");
        let digest2 = Sha256::digest(b"/path/to/my-repo");
        assert_eq!(derived_base_port(&digest1), derived_base_port(&digest2));

        let port = derived_base_port(&digest1);
        assert!((20000..=60000).contains(&port));
        assert_eq!(port % 20, 0);
    }

    #[test]
    fn test_resolve_base_port_fallbacks() {
        let digest = Sha256::digest(b"/path/to/workspace");
        let preferred = derived_base_port(&digest);

        // When preferred is free
        let (port, warn) = resolve_base_port_with_checker(None, None, &digest, |_| true).unwrap();
        assert_eq!(port, preferred);
        assert!(warn.is_none());

        // When preferred is squatted
        let (port, warn) =
            resolve_base_port_with_checker(None, None, &digest, |p| p != preferred).unwrap();
        assert_eq!(port, preferred + 1);
        assert!(warn.unwrap().contains("already in use"));

        // When config base_port is specified and squatted
        let (port, warn) =
            resolve_base_port_with_checker(None, Some(3000), &digest, |p| p != 3000).unwrap();
        assert_eq!(port, 3001);
        assert!(warn.unwrap().contains("already in use"));

        // When CLI port is specified, even if busy, it is respected
        let (port, warn) =
            resolve_base_port_with_checker(Some(8080), None, &digest, |p| p != 8080).unwrap();
        assert_eq!(port, 8080);
        assert!(warn.unwrap().contains("already in use"));
    }

    #[test]
    fn test_allocate_ports_independent_when_offset_squatted() {
        let mut config = Config {
            name: "test".into(),
            base_port: None,
            compose_file: None,
            dev_command: None,
            env_file: ".env".into(),
            copy_files: vec![],
            services: Default::default(),
            env_template: Default::default(),
        };

        // Add custom service with offset 1
        config.services.custom.insert(
            "mail".into(),
            crate::config::CustomServiceConfig {
                image: "mail:latest".into(),
                port_offset: Some(1),
                target_port: None,
                environment: Default::default(),
                command: vec![],
                volumes: vec![],
            },
        );

        // Scenario 1: Port 4001 is free
        let (ports, warnings) =
            allocate_ports_with_checker(&config, 4000, |_| true).unwrap();
        assert_eq!(ports.get("base"), Some(&4000));
        assert_eq!(ports.get("mail"), Some(&4001));
        assert!(warnings.is_empty());

        // Scenario 2: Port 4001 is squatted!
        let (ports, warnings) =
            allocate_ports_with_checker(&config, 4000, |p| p != 4001).unwrap();
        assert_eq!(ports.get("base"), Some(&4000));
        assert_eq!(ports.get("mail"), Some(&4002));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("dynamically allocated port 4002"));
    }
}
