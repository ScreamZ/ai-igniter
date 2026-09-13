use crate::cli::EnvArgs;
use crate::context::WorkspaceContext;
use crate::env_writer::{EnvWriter, quote_env_value};
use anyhow::Result;

pub fn execute_env(ctx: &WorkspaceContext, args: &EnvArgs) -> Result<()> {
    if args.write {
        EnvWriter::write_workspace_env(ctx)?;
    } else {
        let (computed, warnings) = EnvWriter::compute_env(ctx);
        EnvWriter::print_warnings(&warnings);
        for (k, v) in computed {
            println!("{}={}", k, quote_env_value(&v));
        }
    }
    Ok(())
}
