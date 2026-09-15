use crate::context::{WorkspaceContext, format_placeholders, render_template};
use anyhow::{Context, Result};
use colored::Colorize;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const BEGIN_MARKER: &str = "# --- Managed by ai-igniter ---";
pub const END_MARKER: &str = "# --- End Managed by ai-igniter ---";

/// Seeds configured files from the root checkout into a worktree without overwriting local files.
pub fn copy_workspace_files(ctx: &WorkspaceContext) -> Result<()> {
    if ctx.root_path == ctx.workspace_path {
        return Ok(());
    }

    for rule in &ctx.config.copy_files {
        copy_workspace_file(ctx, &rule.from, &rule.to)?;
    }

    Ok(())
}

fn copy_workspace_file(ctx: &WorkspaceContext, from: &Path, to: &Path) -> Result<()> {
    let source = ctx.root_path.join(from);
    let destination = ctx.workspace_path.join(to);

    // `symlink_metadata` considers a dangling symlink an existing local destination too.
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("Failed to inspect {:?}", destination)),
    }

    if !source.exists() {
        eprintln!(
            "{} Warning: source file {} does not exist; skipping copy to {}",
            "[env]".yellow().bold(),
            source.display(),
            destination.display()
        );
        return Ok(());
    }
    if !source.is_file() {
        anyhow::bail!("Configured copy source {:?} is not a file", source);
    }

    let parent = destination
        .parent()
        .expect("workspace-relative destination always has a parent");
    fs::create_dir_all(parent).with_context(|| format!("Failed to create {:?}", parent))?;
    let mut input = fs::File::open(&source)
        .with_context(|| format!("Failed to open configured copy source {:?}", source))?;
    let mut output = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
    {
        Ok(file) => file,
        // Another invocation seeded it after our initial check. Never overwrite it.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("Failed to create {:?}", destination)),
    };
    if let Err(e) = io::copy(&mut input, &mut output) {
        let _ = fs::remove_file(&destination);
        return Err(e).with_context(|| format!("Failed to copy {:?} to {:?}", source, destination));
    }

    println!(
        "{} Seeded {} from {}",
        "[env]".blue().bold(),
        destination.display().to_string().cyan(),
        source.display()
    );
    Ok(())
}

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
        copy_workspace_files(ctx)?;
        let env_path = ctx.workspace_path.join(ctx.config.env_file());

        let existing_content = match fs::read_to_string(&env_path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e).with_context(|| format!("Failed to read {:?}", env_path)),
        };

        let (computed, warnings) = Self::compute_env(ctx);
        Self::print_warnings(&warnings);

        // Write to a uniquely named sibling, then rename, so readers never see partial content.
        atomic_write(&env_path, &merge_env(&existing_content, &computed))?;

        println!(
            "{} Updated environment variables in {}",
            "[env]".blue().bold(),
            env_path.display().to_string().cyan()
        );

        Ok(())
    }
}

fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .expect("workspace-relative environment path always has a parent");
    fs::create_dir_all(parent).with_context(|| format!("Failed to create {:?}", parent))?;

    let filename = path
        .file_name()
        .expect("workspace-relative environment path has a filename")
        .to_string_lossy();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    for attempt in 0..100 {
        let tmp_path = parent.join(format!(
            ".{filename}.ai-igniter.{}.{}.{attempt}.tmp",
            std::process::id(),
            nanos
        ));
        let file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("Failed to create {:?}", tmp_path)),
        };

        let write_result = (|| -> Result<()> {
            let mut file = file;
            file.write_all(contents.as_bytes())
                .with_context(|| format!("Failed to write {:?}", tmp_path))?;
            file.sync_all()
                .with_context(|| format!("Failed to sync {:?}", tmp_path))?;
            drop(file);
            fs::rename(&tmp_path, path).with_context(|| format!("Failed to replace {:?}", path))?;
            Ok(())
        })();

        if write_result.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        return write_result;
    }

    anyhow::bail!(
        "Could not create a unique temporary file next to {:?}",
        path
    )
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
    use crate::config::Config;
    use std::path::PathBuf;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let unique = format!(
                "ai-igniter-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let path = std::env::temp_dir().join(unique);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn workspace_context(root: PathBuf, workspace: PathBuf, config_toml: &str) -> WorkspaceContext {
        let config: Config = toml::from_str(config_toml).unwrap();
        WorkspaceContext::from_parts(
            workspace,
            root.clone(),
            root.join("ai-igniter.toml"),
            config,
            Some(4300),
            true,
        )
        .unwrap()
    }

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

    #[test]
    fn seeding_uses_configured_copy_rules_and_manages_env_file() {
        let temp = TestDir::new("custom-env-copy");
        let root = temp.0.join("root");
        let workspace = temp.0.join("worktree");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::write(root.join(".env"), "USER_SECRET=kept\n").unwrap();
        let ctx = workspace_context(
            root,
            workspace.clone(),
            r#"
name = "p"
env_file = "config/.env.local"
copy_files = [{ from = ".env", to = "config/.env.local" }]
[env_template]
MANAGED = "value"
"#,
        );

        EnvWriter::write_workspace_env(&ctx).unwrap();

        assert_eq!(
            fs::read_to_string(workspace.join("config/.env.local")).unwrap(),
            format!("USER_SECRET=kept\n\n{BEGIN_MARKER}\nMANAGED=value\n{END_MARKER}\n")
        );
    }

    #[test]
    fn copies_mixed_rules_without_overwriting_existing_destinations() {
        let temp = TestDir::new("mixed-copy");
        let root = temp.0.join("root");
        let workspace = temp.0.join("worktree");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(workspace.join("nested")).unwrap();
        fs::write(root.join(".env"), "ROOT=one\n").unwrap();
        fs::write(root.join(".env.test"), "TEST=one\n").unwrap();
        fs::write(workspace.join("nested/.env.local"), "LOCAL=kept\n").unwrap();
        let ctx = workspace_context(
            root,
            workspace.clone(),
            r#"
name = "p"
env_file = ".env"
copy_files = [".env.test", { from = ".env", to = "nested/.env.local" }]
"#,
        );

        copy_workspace_files(&ctx).unwrap();

        assert_eq!(
            fs::read_to_string(workspace.join(".env.test")).unwrap(),
            "TEST=one\n"
        );
        assert_eq!(
            fs::read_to_string(workspace.join("nested/.env.local")).unwrap(),
            "LOCAL=kept\n"
        );
    }

    #[test]
    fn empty_copy_list_does_not_copy_anything() {
        let temp = TestDir::new("empty-copy-list");
        let root = temp.0.join("root");
        let workspace = temp.0.join("worktree");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::write(root.join(".env"), "ROOT=one\n").unwrap();
        let ctx = workspace_context(
            root,
            workspace.clone(),
            "name = \"p\"\nenv_file = \".env\"\ncopy_files = []",
        );

        copy_workspace_files(&ctx).unwrap();

        assert!(!workspace.join(".env").exists());
    }

    #[test]
    fn does_not_copy_when_root_is_the_workspace() {
        let temp = TestDir::new("local-workspace");
        fs::write(temp.0.join(".env"), "ROOT=one\n").unwrap();
        let ctx = workspace_context(
            temp.0.clone(),
            temp.0.clone(),
            "name = \"p\"\nenv_file = \".env\"\ncopy_files = [\".env\"]",
        );

        copy_workspace_files(&ctx).unwrap();

        assert_eq!(
            fs::read_to_string(temp.0.join(".env")).unwrap(),
            "ROOT=one\n"
        );
    }
}
