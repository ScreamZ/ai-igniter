<div align="center">

# 🚀 ai-igniter

**Stop fighting port conflicts across git worktrees. Start coding.**

*Lightning-fast, standalone workspace & service orchestrator for AI worktrees and parallel development (Cursor, Paseo, Conductor, Orca, and CLI).*

[![Rust](https://img.shields.io/badge/built_with-Rust-orange.svg?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![Docker](https://img.shields.io/badge/powered_by-Docker-blue.svg?style=flat-square&logo=docker)](https://www.docker.com/)
[![License](https://img.shields.io/badge/license-MIT-green.svg?style=flat-square)](LICENSE)

<br />

![ai-igniter demo](./assets/demo.gif)

</div>

---

## 💡 Why ai-igniter?

When using **AI coding agents** (Cursor Agent, Claude Code, Paseo, Conductor) or working across **multiple Git worktrees** in parallel, your local development quickly breaks:

- 💥 **Port Conflicts:** Multiple branches try to bind to `3000`, `5432`, or `9000` simultaneously.
- 🤯 **Dirty `.env` Files:** Manual updates of database URLs and credentials per worktree are fragile and tedious.
- 🐌 **Heavy Orchestration Scripts:** Fragile bash or Node glue-scripts to manage Docker containers, healthchecks, and migrations.

**`ai-igniter` solves this completely.** It replaces complex scripts with a single, standalone **Rust binary** that orchestrates isolated services, resolves dynamic ports, runs migrations & seeds, and injects clean environment variables automatically.

---

## 🏛️ The Three Pillars

### 1. 🚦 Automatic Port Resolution & Isolation
- **Dynamic Port Mapping:** Every worktree gets its own isolated port space derived from its path or provided by an orchestrator (`Paseo`, `Conductor`, etc.).
- **Safe Port Reclaiming:** Ports held by inactive `ai-igniter` containers are automatically freed without losing your volumes. Containers from external tools are left untouched.
- **Anti-Hijacking Resolution:** Strict directory and worktree resolution guarantees commands *never* accidentally target another project's worktree.

### 2. 🧱 Zero-Config "À la Carte" Services
Enable only what your project needs via `ai-igniter.toml`:
- **PostgreSQL:** Instant container with TCP health checks, optional secondary (E2E) database, automatic migration runners, and one-time initial seeders.
- **Garage S3:** Self-contained S3-compatible storage with automatic bucket creation, access keys, permissions, CORS, and website hosting endpoints.
- **Custom Docker Services:** Any Docker image (e.g. Mailpit, Redis, Meilisearch) configured with custom ports and volumes in seconds.

### 3. 📝 Atomic Environment Management
- Injects evaluated service URLs, credentials, and ports into a dedicated section of your target environment file (e.g., `.env` or `.env.local`).
- Preserves all your existing custom variables outside the managed section.
- One-time seed copying (`copy_files`) from your main repository checkout when creating new worktrees.

> 📖 **Deep Dive:** Want to understand the resolution hierarchy, deterministic port hashing, and anti-hijacking system? Check out the [Workspace Resolution & Port Allocation Guide](file:///Users/screamz/dev-workspace/perso/ai-tools/docs/workspace-resolution.md).

---

## 📦 Installation

Install globally using `cargo`:

```bash
cargo install --path .
```

Or copy the compiled binary to your `PATH`:

```bash
# macOS / Linux
sudo cp target/release/ai-igniter /usr/local/bin/
```

---

## ⚡ Quick Start

### 1. Initialize your project

Run the interactive wizard in your repository root:

```bash
ai-igniter init
```

This guides you through selecting your orchestrator, configuring services (PostgreSQL, S3), and creates an `ai-igniter.toml` configuration file.

### 2. Start developing

Spin up all workspace services, run migrations/seeds, update `.env`, and start your dev server in one step:

```bash
ai-igniter dev
```

> **Tip:** When you press `Ctrl+C`, `ai-igniter` gracefully stops all associated Docker containers and child processes.

### 3. Teardown when finished

When you delete or archive a worktree, clean up all associated containers, networks, and volumes cleanly:

```bash
ai-igniter teardown
```

---

## ⚙️ Configuration (`ai-igniter.toml`)

### Minimal Example

Here is all you need for a full Next.js / Node app with PostgreSQL:

```toml
name = "my-awesome-app"
env_file = ".env"
copy_files = []
dev_command = "bun run dev"

[services.postgres]
enabled = true
port_offset = 1
database = "app_db"
user = "app_user"
password = "app_password"
migrate_command = "bun run db:migrate"

[env_template]
DATABASE_URL = "{{services.postgres.url}}"
APP_URL = "http://localhost:{{ports.base}}"
```

<details>
<summary><b>🔍 View Full Comprehensive Configuration Reference</b></summary>

<br>

```toml
name = "my-project"
env_file = ".env"                          # Target file receiving managed env vars
copy_files = []                            # Files to seed from root checkout on first use
dev_command = "bun run dev"                # Command executed after services are healthy
# base_port = 3000                         # Optional fixed base port
# compose_file = "docker-compose.dev.yml"  # Optional: use custom compose file

# Orchestrator environment variable hooks
[orchestrator]
port_env = "PASEO_PORT"
root_env = "PASEO_SOURCE_CHECKOUT_PATH"

# Built-in PostgreSQL
[services.postgres]
enabled = true
port_offset = 1
image = "postgres:16"
database = "my-project"
user = "my-project"
password = "my-project"
e2e_database = "my-project_e2e"
migrate_command = "bun run db:migrate"
seed_command = "bun run db:seed"
# seed_check_sql = "SELECT count(*) FROM users"

# Built-in Garage S3
[services.garage]
enabled = true
port_offset = 3
web_port_offset = 4
image = "dxflrs/garage:v2.4.1"
access_key = "my-project-local-access-key"
secret_key = "my-project-local-secret-key-change-me"
buckets = ["my-project-assets"]
website_buckets = ["my-project-assets"]
website_root_domain = ".web.localhost"

# Custom Services (e.g. Mailpit)
[services.custom.mailpit]
image = "axllent/mailpit"
port_offset = 8
target_port = 8025
environment = { MP_MAX_MESSAGES = "500" }
command = ["--smtp-auth-accept-any"]
volumes = ["./.mailpit:/data", "mailpit-cache:/cache"]

# Dynamic environment template
[env_template]
DATABASE_URL = "{{services.postgres.url}}"
DATABASE_MIGRATION_URL = "{{services.postgres.url}}"
E2E_DATABASE_URL = "{{services.postgres.e2e_url}}"
S3_ENDPOINT = "{{services.garage.endpoint}}"
S3_ACCESS_KEY_ID = "{{services.garage.access_key}}"
S3_SECRET_ACCESS_KEY = "{{services.garage.secret_key}}"
S3_REGION = "{{services.garage.region}}"
S3_BUCKET = "my-project-assets"
S3_PUBLIC_URL = "http://my-project-assets{{services.garage.website_root_domain}}:{{services.garage.web_port}}"
MAIL_UI_URL = "http://localhost:{{services.mailpit.port}}"
APP_URL = "http://localhost:{{ports.base}}"
```

#### Available Template Placeholders

| Placeholder | Description |
| :--- | :--- |
| `{{ports.base}}` | Primary workspace application port |
| `{{ports.<name>}}` / `{{ports.base + N}}` | Allocated port for a service, or base + arithmetic offset |
| `{{services.postgres.url}}` / `e2e_url` | Full PostgreSQL connection URLs with URL-encoded credentials |
| `{{services.postgres.port}}` / `host` / `user` / `password` / `database` | Granular PostgreSQL connection details |
| `{{services.garage.endpoint}}` / `access_key` / `secret_key` / `web_port` | Garage S3 connection & web endpoints |
| `{{services.<name>.port}}` | Host port for custom service declared in `[services.custom.<name>]` |
| `{{workspace.slug}}` / `{{workspace.hash}}` / `{{workspace.path}}` | Current workspace identity metadata |

</details>

---

## 🤖 Orchestrator Integrations

### Paseo (`paseo.json`)
```json
{
  "worktree": {
    "teardown": "ai-igniter teardown"
  },
  "scripts": {
    "services": {
      "type": "service",
      "command": "ai-igniter dev"
    }
  }
}
```

### Conductor / Cursor Worktrees / Standalone CLI
Add `ai-igniter dev` to your worktree initialization hook or launch task, and `ai-igniter teardown` on worktree deletion.

---

## 🛠️ CLI Command Reference

| Command | Description |
| :--- | :--- |
| `ai-igniter init` | Interactive wizard to initialize `ai-igniter.toml`. |
| `ai-igniter dev` *(alias: `up`)* | Start services, run migrations/seeds, write `.env`, and launch dev command. |
| `ai-igniter dev --reset` | Reset database/storage volumes to fresh state, re-seed, and start. |
| `ai-igniter dev --no-command` | Run and supervise background Docker services without launching `dev_command`. |
| `ai-igniter dev -- <cmd>` | Override the default `dev_command` (e.g. `ai-igniter dev -- cargo run`). |
| `ai-igniter teardown` *(alias: `down`)* | Stop and remove this workspace's containers, networks, and volumes. |
| `ai-igniter status` | Inspect allocated ports and container health for the current workspace. |
| `ai-igniter env` | Print or update (`--write`) evaluated environment variables. |

---

## 🏗️ Architecture & Extensibility

`ai-igniter` is built in Rust with modularity in mind:

```
src/
├── cli.rs               # Clap definitions & CLI arguments
├── commands/            # init, dev, teardown, status, env
├── config.rs            # TOML parsing, validation & defaults
├── context.rs           # Workspace & port resolution engine
├── docker/              # Dynamic Compose generator & port reclaimer
├── env_writer.rs        # Atomic .env delimiter engine
├── services/            # Built-in providers (PostgreSQL, Garage S3, Custom)
└── supervisor.rs        # Process supervisor & signal trap (SIGINT/SIGTERM)
```

### Adding a new built-in service
1. Create `src/services/<service_name>.rs` implementing the `ServiceProvider` trait.
2. Register it in `BUILTIN_SERVICES` in [`src/services/mod.rs`](file:///Users/screamz/dev-workspace/perso/ai-tools/src/services/mod.rs).
3. All commands (`init`, `dev`, `status`, etc.) will automatically support your new service!

---

## 📄 License

MIT © [ScreamZ](https://github.com/ScreamZ)
