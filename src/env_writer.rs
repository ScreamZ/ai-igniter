use crate::context::{WorkspaceContext, format_placeholders, render_template};
use anyhow::{Context, Result};
use colored::Colorize;
use std::collections::BTreeMap;
use std::fs;

pub const BEGIN_MARKER: &str = "# --- Managed by ai-igniter ---";
pub const END_MARKER: &str = "# --- End Managed by ai-igniter ---";

pub struct EnvWriter;

impl EnvWriter {
    /// Evaluates `env_template`; variables referencing unknown placeholders (e.g. a disabled service) are skipped with a warning.
    pub fn compute_env(ctx: &WorkspaceContext) -> (BTreeMap<String, String>, Vec<String>) {
        let vars = ctx.template_vars();
        let mut computed = BTreeMap::new();
        let mut warnings = Vec::new();

        for (key, tmpl) in &ctx.config.env_template {
            match render_template(tmpl, &vars) {
                Ok(value) => {
                    computed.insert(key.clone(), value);
                }
                Err(unknown) => warnings.push(format!(
                    "Skipping {key}: unknown placeholder(s) {} (is the service enabled?)",
                    format_placeholders(&unknown)
                )),
            }
        }

        (computed, warnings)
    }

    pub fn print_warnings(warnings: &[String]) {
        for warning in warnings {
            eprintln!("{} Warning: {}", "[env]".yellow().bold(), warning);
        }
    }

    pub fn write_workspace_env(ctx: &WorkspaceContext) -> Result<()> {
        let env_path = ctx.workspace_path.join(".env");
        let root_env_path = ctx.root_path.join(".env");

        // Seed a new worktree with the source checkout's .env
        if !env_path.exists()
            && root_env_path.exists()
            && ctx.root_path != ctx.workspace_path
            && let Err(e) = fs::copy(&root_env_path, &env_path)
        {
            eprintln!(
                "{} Warning: could not copy {:?}: {}",
                "[env]".yellow().bold(),
                root_env_path,
                e
            );
        }

        let existing_content = match fs::read_to_string(&env_path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("Failed to read {:?}", env_path)),
        };

        let (computed, warnings) = Self::compute_env(ctx);
        Self::print_warnings(&warnings);

        // Write to a sibling temp file then rename, so readers never see a partial .env
        let tmp_path = ctx.workspace_path.join(".env.ai-igniter.tmp");
        fs::write(&tmp_path, merge_env(&existing_content, &computed))
            .with_context(|| format!("Failed to write {:?}", tmp_path))?;
        fs::rename(&tmp_path, &env_path)
            .with_context(|| format!("Failed to replace {:?}", env_path))?;

        println!(
            "{} Updated environment variables in {}",
            "[env]".blue().bold(),
            env_path.display().to_string().cyan()
        );

        Ok(())
    }
}

/// Replaces the managed block in `existing` (in place, or appended) and drops managed keys defined elsewhere.
pub fn merge_env(existing: &str, computed: &BTreeMap<String, String>) -> String {
    #[derive(PartialEq)]
    enum Section {
        Before,
        Managed,
        After,
    }

    let mut section = Section::Before;
    let mut before: Vec<&str> = Vec::new();
    let mut after: Vec<&str> = Vec::new();

    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed == BEGIN_MARKER {
            section = Section::Managed;
            continue;
        }
        if section == Section::Managed {
            // Legacy files have no end marker: the block then runs to the end of the file
            if trimmed == END_MARKER {
                section = Section::After;
            }
            continue;
        }
        if defines_managed_key(line, computed) {
            continue;
        }
        match section {
            Section::Before => before.push(line),
            _ => after.push(line),
        }
    }

    let mut out: Vec<String> = before.iter().map(|l| l.to_string()).collect();
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    if !out.is_empty() {
        out.push(String::new());
    }
    out.push(BEGIN_MARKER.to_string());
    out.extend(
        computed
            .iter()
            .map(|(k, v)| format!("{k}={}", quote_env_value(v))),
    );
    out.push(END_MARKER.to_string());

    if let Some(first) = after.iter().position(|l| !l.trim().is_empty()) {
        out.push(String::new());
        out.extend(after[first..].iter().map(|l| l.to_string()));
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }

    let mut content = out.join("\n");
    content.push('\n');
    content
}

fn defines_managed_key(line: &str, computed: &BTreeMap<String, String>) -> bool {
    let line = line.trim_start();
    if line.starts_with('#') {
        return false;
    }
    let line = line.strip_prefix("export ").unwrap_or(line);
    line.split_once('=')
        .is_some_and(|(key, _)| computed.contains_key(key.trim()))
}

/// Quotes values dotenv parsers would otherwise truncate or expand; plain values stay unquoted.
pub fn quote_env_value(value: &str) -> String {
    let needs_quotes = value
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '#' | '"' | '\'' | '$' | '\\' | '`'));
    if !needs_quotes {
        value.to_string()
    } else if !value.contains('\'') {
        format!("'{value}'")
    } else {
        format!(
            "\"{}\"",
            value
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('$', "\\$")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn computed(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn appends_block_to_file_without_one() {
        let out = merge_env("FOO=1\n\n", &computed(&[("DATABASE_URL", "postgres://x")]));
        assert_eq!(
            out,
            format!("FOO=1\n\n{BEGIN_MARKER}\nDATABASE_URL=postgres://x\n{END_MARKER}\n")
        );
    }

    #[test]
    fn replaces_block_in_place_and_keeps_user_lines_after_it() {
        let existing = format!("A=1\n{BEGIN_MARKER}\nOLD=1\n{END_MARKER}\n\nB=2\n");
        let out = merge_env(&existing, &computed(&[("NEW", "2")]));
        assert_eq!(
            out,
            format!("A=1\n\n{BEGIN_MARKER}\nNEW=2\n{END_MARKER}\n\nB=2\n")
        );
    }

    #[test]
    fn legacy_block_without_end_marker_runs_to_eof() {
        let existing = format!("A=1\n\n{BEGIN_MARKER}\nOLD=1\n");
        let out = merge_env(&existing, &computed(&[("NEW", "2")]));
        assert_eq!(out, format!("A=1\n\n{BEGIN_MARKER}\nNEW=2\n{END_MARKER}\n"));
    }

    #[test]
    fn drops_managed_keys_defined_outside_the_block() {
        let existing = "export S3_BUCKET=old\nS3_BUCKET = old\n# S3_BUCKET=comment kept\nOTHER=1\n";
        let out = merge_env(existing, &computed(&[("S3_BUCKET", "new")]));
        assert_eq!(
            out,
            format!(
                "# S3_BUCKET=comment kept\nOTHER=1\n\n{BEGIN_MARKER}\nS3_BUCKET=new\n{END_MARKER}\n"
            )
        );
    }

    #[test]
    fn is_idempotent() {
        let vars = computed(&[("A", "x y"), ("B", "1")]);
        let once = merge_env("USER=me\n", &vars);
        assert_eq!(merge_env(&once, &vars), once);
    }

    #[test]
    fn quotes_only_when_needed() {
        assert_eq!(
            quote_env_value("postgresql://u:p@127.0.0.1:5432/db"),
            "postgresql://u:p@127.0.0.1:5432/db"
        );
        assert_eq!(quote_env_value("a b#c$d"), "'a b#c$d'");
        assert_eq!(quote_env_value("it's \"$x\""), "\"it's \\\"\\$x\\\"\"");
    }
}
