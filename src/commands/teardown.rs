use crate::context::WorkspaceContext;
use crate::docker::DockerCompose;
use anyhow::{Context, Result};
use std::fs;
use std::io::ErrorKind;

/// Only removes this workspace's own project: other projects are never touched, even if they share ports.
pub fn execute_teardown(ctx: &WorkspaceContext) -> Result<()> {
    let compose = DockerCompose::new(ctx)?;
    compose.down(true)?;

    if ctx.config.compose_file.is_none() {
        match fs::remove_dir_all(ctx.igniter_dir()) {
            Err(e) if e.kind() != ErrorKind::NotFound => {
                return Err(e).with_context(|| format!("Failed to remove {:?}", ctx.igniter_dir()));
            }
            _ => {}
        }
    }
    Ok(())
}
