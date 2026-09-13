use crate::context::WorkspaceContext;
use crate::docker::compose::MANAGED_LABEL;
use colored::Colorize;
use serde_json::Value;
use std::collections::BTreeSet;
use std::process::Command;

#[derive(Debug, PartialEq)]
enum Action {
    /// Another ai-igniter project holds one of our ports: stop it, keeping its volumes.
    StopProject { project: String, port: u16 },
    /// A container ai-igniter did not create holds one of our ports: never touched.
    WarnForeign { container: String, port: u16 },
}

/// Frees this workspace's ports from other ai-igniter projects (containers only, volumes are kept).
pub fn reclaim_stale_ports(ctx: &WorkspaceContext) {
    let Some(containers) = running_containers() else {
        return; // Docker unavailable or nothing running; `compose up` reports real problems
    };
    let ports: BTreeSet<u16> = ctx.port_allocations.values().copied().collect();

    for action in plan(&containers, &ctx.compose_project, &ports) {
        match action {
            Action::StopProject { project, port } => {
                eprintln!(
                    "{} Stopping ai-igniter project '{}' which holds port {} (its volumes are kept)",
                    "[reclaim]".yellow().bold(),
                    project,
                    port
                );
                let stopped = Command::new("docker")
                    .args(["compose", "-p", &project, "down", "--remove-orphans"])
                    .output()
                    .is_ok_and(|o| o.status.success());
                if !stopped {
                    eprintln!("{} Warning: failed to stop project '{}'", "[reclaim]".yellow().bold(), project);
                }
            }
            Action::WarnForeign { container, port } => eprintln!(
                "{} Warning: port {} is used by container '{}', which is not managed by ai-igniter; leaving it untouched",
                "[reclaim]".yellow().bold(),
                port,
                container
            ),
        }
    }
}

fn running_containers() -> Option<Vec<Value>> {
    let ps = Command::new("docker").args(["ps", "-q"]).output().ok().filter(|o| o.status.success())?;
    let ps_stdout = String::from_utf8_lossy(&ps.stdout);
    let ids: Vec<&str> = ps_stdout.split_whitespace().collect();
    if ids.is_empty() {
        return None;
    }
    let inspect = Command::new("docker")
        .arg("inspect")
        .args(&ids)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    serde_json::from_slice(&inspect.stdout).ok()
}

fn plan(containers: &[Value], own_project: &str, ports: &BTreeSet<u16>) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut stopping = BTreeSet::new();

    for container in containers {
        let labels = &container["Config"]["Labels"];
        let project = labels["com.docker.compose.project"].as_str().unwrap_or("");
        if !project.is_empty() && project == own_project {
            continue;
        }
        let Some(port) = host_ports(container).into_iter().find(|p| ports.contains(p)) else {
            continue;
        };

        if labels[MANAGED_LABEL] == "true" && !project.is_empty() {
            if stopping.insert(project) {
                actions.push(Action::StopProject { project: project.to_string(), port });
            }
        } else {
            let name = container["Name"].as_str().unwrap_or("unknown").trim_start_matches('/');
            actions.push(Action::WarnForeign { container: name.to_string(), port });
        }
    }
    actions
}

fn host_ports(container: &Value) -> Vec<u16> {
    container["HostConfig"]["PortBindings"]
        .as_object()
        .into_iter()
        .flat_map(|bindings| bindings.values())
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|binding| binding["HostPort"].as_str()?.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn container(name: &str, project: Option<&str>, managed: bool, host_port: u16) -> Value {
        let mut labels = json!({});
        if let Some(project) = project {
            labels["com.docker.compose.project"] = json!(project);
        }
        if managed {
            labels[MANAGED_LABEL] = json!("true");
        }
        json!({
            "Name": format!("/{name}"),
            "Config": { "Labels": labels },
            "HostConfig": { "PortBindings": { "5432/tcp": [{ "HostIp": "", "HostPort": host_port.to_string() }] } },
        })
    }

    #[test]
    fn stops_only_other_managed_projects_and_warns_for_foreign_containers() {
        let containers = [
            container("mine-postgres-1", Some("mine"), true, 4001),
            container("other-postgres-1", Some("other"), true, 4001),
            container("other-garage-1", Some("other"), true, 4003),
            container("legacy-db-1", Some("legacy"), false, 4003),
            container("my-local-pg", None, false, 4005),
            container("unrelated", Some("x"), true, 9999),
        ];
        let ports = BTreeSet::from([4001, 4003, 4005]);
        assert_eq!(
            plan(&containers, "mine", &ports),
            [
                Action::StopProject { project: "other".into(), port: 4001 },
                Action::WarnForeign { container: "legacy-db-1".into(), port: 4003 },
                Action::WarnForeign { container: "my-local-pg".into(), port: 4005 },
            ]
        );
    }
}
