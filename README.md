# 🚀 ai-igniter

> **Lightning-fast, extensible workspace & service orchestrator for AI worktrees** (Paseo, Conductor, Orca, Cursor, etc.).

`ai-igniter` replaces complex and brittle bash/node scripts with a single, standalone **Rust binary**. It provides complete lifecycle management for parallel git worktrees, isolated Docker services (PostgreSQL, Garage S3, Custom), and dynamic environment variable synchronization into `.env`.

---

## ✨ Key Features

- 🏎️ **Lightweight & Fast:** Single standalone binary, no runtime besides Docker.
- 🧩 **Services "À la carte":** Toggle and configure services per project via `ai-igniter.toml`. The Compose file is regenerated on every run, so enabling, disabling or reconfiguring a service takes effect on the next `dev`:
  - **PostgreSQL**: Container with TCP healthcheck, secondary (e2e) database, migrations runner, and a seed that runs once per fresh volume.
  - **Garage S3**: Default key, bucket creation, permissions, website hosting, and S3 CORS configuration without external scripts.
  - **Custom Services**: Any Docker image with ports, environment, command and volumes declared in TOML.
- 🔄 **Orchestrator Agnostic:** Maps dynamic ports and source checkouts from **Paseo**, **Conductor**, **Orca**, or generic `WORKSPACE_*` variables. Without an orchestrator port, each worktree gets a stable port derived from its path.
- 🛡️ **Safe Port Reclaiming:** Ports held by another *ai-igniter* project are freed (containers stopped, volumes kept). Containers ai-igniter did not create are never touched.
- 🎯 **Dedicated Service Keep-Alive:** Keeps services running in foreground, exits with an error if a service dies, and stops Docker on `Ctrl+C` / `SIGTERM` / `SIGHUP` — including during startup.
- 📝 **Atomic `.env` Management:** Interpolates service URLs, credentials, and ports into a delimited section of `.env`, preserving every other line.

---

## 📦 Installation

To install `ai-igniter` globally on your machine:

```bash
cargo install --path .
```

Or copy the compiled release binary directly to your `PATH`:

```bash
cp target/release/ai-igniter ~/.cargo/bin/
# or
sudo cp target/release/ai-igniter /usr/local/bin/
```

---

## 🚀 Quick Start

### 1. Initialize a Project

In any repository root:

```bash
ai-igniter init
```

The interactive wizard prompts you for:
- Project name (normalized to lowercase letters, digits, `-` and `_`)
- Orchestrator (Paseo, Conductor, Orca, Custom)
- Services to enable (PostgreSQL, Garage S3)
- If PostgreSQL is selected: migration and seed commands (with defaults, or empty to skip)

This generates `ai-igniter.toml` and adds `.igniter/` to `.gitignore`. Use `--non-interactive` for defaults and `--force` to overwrite an existing file.

Commit `ai-igniter.toml` so every worktree shares it. If it stays untracked, worktrees fall back to the main checkout's copy.

### 2. Integration with Paseo (`paseo.json`)

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

### 3. Integration with Conductor / Standalone

Start services and generate `.env`:
```bash
ai-igniter dev
```
And stop/teardown when done:
```bash
ai-igniter teardown
```

---

## 🛠️ CLI Commands

| Command | Description |
| :--- | :--- |
| `ai-igniter init` | Interactive wizard to pick services, dev command, and initialize `ai-igniter.toml`. |
| `ai-igniter dev` *(alias: `up`)* | Start workspace services, create buckets/databases, run migrations and the first seed, update `.env`, and optionally run `dev_command` (e.g. `bun run dev`) or keep services alive in foreground. Stops child process and Docker on exit (`Ctrl+C`, `SIGTERM`, `SIGHUP`). |
| `ai-igniter dev --reset` | Wipe volumes, recreate fresh services, re-run migrations/seeds, then start dev. |
| `ai-igniter dev --no-command` | Start and supervise services in the foreground, skipping any configured `dev_command`. |
| `ai-igniter dev -- <cmd>` | Run an ad-hoc dev command overriding `dev_command` (e.g. `ai-igniter dev -- bun run dev`). |
| `ai-igniter teardown` *(aliases: `down`, `archive`)* | Delete this workspace's containers, volumes, networks and `.igniter/`. Other projects are never touched. |
| `ai-igniter status` | Display allocated ports and every container of the project with its state and health. |
| `ai-igniter env` | Display evaluated environment variables (use `--write` to write them to `.env`). |

Global flags: `--dir`, `--root`, `--port`, `--config`.

---

## 📝 Dynamic Environment Variables & Template Interpolation

One of `ai-igniter`'s core responsibilities is generating the appropriate `.env` variables for your application, because **ports are dynamic** per worktree or orchestrator.

### How it works:
1. Whenever `ai-igniter dev` (or `ai-igniter env --write`) runs, it evaluates the `[env_template]` table in `ai-igniter.toml`.
2. It replaces placeholders with the ports and credentials resolved for the current worktree. A variable referencing a disabled service is skipped with a warning.
3. On a worktree's first run, `.env` is seeded from the source checkout's `.env`.
4. It atomically updates `.env` inside a delimited section:
   ```bash
   USER_SECRET=kept-as-is

   # --- Managed by ai-igniter ---
   DATABASE_URL=postgresql://my-project:my-project@127.0.0.1:3001/my-project
   S3_ENDPOINT=http://localhost:3003
   # --- End Managed by ai-igniter ---

   ANOTHER_USER_VAR=also-kept
   ```
   Lines outside the section are preserved, except duplicate definitions of managed keys. Values containing spaces, `#`, quotes or `$` are quoted.

Migration and seed commands run with the same variables in their environment.

### Available Template Placeholders:

| Placeholder | Meaning |
| :--- | :--- |
| `{{ports.base}}` | Workspace base port (app port) |
| `{{ports.<name>}}`, `{{ports.base + N}}` | Any allocated port (`postgres`, `garage`, `garage_web`, custom names), or base + offset |
| `{{project.name}}`, `{{workspace.slug}}`, `{{workspace.hash}}`, `{{workspace.compose_project}}`, `{{workspace.path}}`, `{{workspace.root}}` | Workspace identity |
| `{{services.postgres.url}}` / `e2e_url` | Full connection URLs, credentials URL-encoded |
| `{{services.postgres.port}}` / `host` / `user` / `password` / `database` / `e2e_database` | Individual PostgreSQL values |
| `{{services.garage.endpoint}}` / `port` / `web_port` / `access_key` / `secret_key` / `region` / `website_root_domain` | Garage S3 values |
| `{{services.<name>.port}}` | Host port of a custom service declared under `[services.custom.<name>]` |

Unknown placeholders are errors in commands and skipped variables in `.env`.

### Adding Custom Variables to `env_template`:

```toml
[env_template]
DATABASE_URL = "{{services.postgres.url}}"
S3_ENDPOINT = "{{services.garage.endpoint}}"

# Custom application-specific variables using the dynamic ports
APP_URL = "http://localhost:{{ports.base}}"
NEXT_PUBLIC_API_URL = "http://localhost:{{ports.base}}/api"
STORYBOOK_URL = "http://localhost:{{ports.base + 10}}"
STORAGE_PUBLIC_URL = "http://my-assets{{services.garage.website_root_domain}}:{{services.garage.web_port}}"
```

---

## ⚙️ Configuration Reference (`ai-igniter.toml`)

```toml
name = "my-project"
# dev_command = "bun run dev"  # Optional: command executed after services are healthy
# base_port = 3000   # Optional fixed base port. See "Port Resolution" below.
# compose_file = "docker-compose.dev.yml"   # Optional: use your own compose file instead of the generated one

# Environment variables provided by the orchestrator
[orchestrator]
port_env = "PASEO_PORT"                   # base port
root_env = "PASEO_SOURCE_CHECKOUT_PATH"   # source checkout (to seed .env in new worktrees)

# Services à la carte (set enabled = false to turn one off)
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
# seed_check_sql = "SELECT count(*) FROM users"   # Optional: skip the seed when > 0

[services.garage]
enabled = true
port_offset = 3
web_port_offset = 4
image = "dxflrs/garage:v2.4.1"
access_key = "my-project-local-access-key"
secret_key = "my-project-local-secret-key-change-me"
buckets = ["my-project-assets"]
website_buckets = ["my-project-assets"]   # created too if missing from `buckets`
website_root_domain = ".web.localhost"

[services.custom.mailpit]
image = "axllent/mailpit"
port_offset = 8
target_port = 8025
environment = { MP_MAX_MESSAGES = "500" }
command = ["--smtp-auth-accept-any"]
volumes = ["./.mailpit:/data", "mailpit-cache:/cache"]   # relative paths resolve from the workspace

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
```

Validation rules:
- Port offsets must be non-zero (0 is the app port) and unique across enabled services.
- Custom service names match `[a-z0-9][a-z0-9_-]*` and cannot be `base`, `postgres`, `garage` or `garage_web`.
- Custom `port_offset` and `target_port` go together.

Values are passed to Docker literally (`$` is escaped). The seed runs once per fresh database volume, tracked with a comment on the database. Set `seed_check_sql` to use your own condition, or `dev --reset` to start over.

### Port Resolution

The base port is the first available of:
1. `--port`
2. `$<orchestrator.port_env>`, then `$WORKSPACE_PORT`, `$PASEO_PORT`, `$CONDUCTOR_PORT` (an invalid value is an error)
3. `base_port` from the config
4. A stable port derived from the workspace path, in `20000..=59980` by steps of 20 (keep offsets below 20)

Each service listens on `base port + offset`.

### Using Your Own Compose File

With `compose_file`, ai-igniter runs that file under the workspace's Compose project. Allocated ports are exported as `IGNITER_<NAME>_PORT` (e.g. `"${IGNITER_POSTGRES_PORT}:5432"`). Built-in post-start steps expect services named `postgres` and `garage`.

---

## 🛡️ Safe Workspace Resolution & Anti-Hijacking

When working across multiple repositories and parallel worktrees, terminal sessions often inherit environment variables (such as `PASEO_WORKTREE_PATH` or `CONDUCTOR_WORKSPACE_PATH`) from concurrent AI sessions or IDE terminals.

`ai-igniter` implements a strict resolution hierarchy (similar to Git and Cargo) to guarantee that commands **never accidentally run in another project's worktree**:

1. **Explicit Flag (`--dir <PATH>`)**:
   An explicit `--dir` argument always has top priority.
2. **Local Project Priority**:
   The nearest directory containing `ai-igniter.toml` is used, searching upward **but never past the current git worktree**. A worktree nested inside the main checkout (e.g. `.claude/worktrees/…`) is never mistaken for the main project. Inside a git worktree without its own config, the worktree is the workspace and the config is read from the main checkout.
3. **External Orchestrator Invocation**:
   Only when invoked from outside any git repository or project, `WORKSPACE_PATH`, `PASEO_WORKTREE_PATH`, `CONDUCTOR_WORKSPACE_PATH` or `ORCA_WORKSPACE_PATH` locate the target worktree.
4. **Strict 1:1 Workspace Isolation**:
   The Docker Compose project (`{project}-{slug}-{hash}`), `.igniter/`, `.env`, and all migration/seed commands are scoped to the resolved workspace.

### Port Reclaiming

Before starting, `dev` inspects running containers bound to the workspace's ports:
- Containers of **another ai-igniter project** (label `ai-igniter.managed=true`) are stopped with `docker compose down`, **keeping their volumes**. That workspace gets its data back on its next `dev`.
- **Any other container** is left untouched with a warning; `docker compose up` then reports the conflict.

`teardown` never reclaims: it only removes its own project.

---

## 🏗️ Architecture & Extensibility

```
src/
├── cli.rs               # Clap command definitions and argument parsing
├── commands/            # Command implementations (init, dev, teardown, status, env)
├── config.rs            # TOML config parsing, validation, and defaults
├── context.rs           # Workspace/root resolution, port allocation, template variables
├── docker/
│   ├── compose.rs       # Compose generation (JSON) & docker compose runner
│   └── reclaim.rs       # Reclaiming ports held by other ai-igniter projects
├── env_writer.rs        # Atomic .env merge
├── services/
│   ├── garage.rs        # Garage S3 setup (buckets, permissions, website, CORS)
│   └── postgres.rs      # Secondary database, migrations, and seed
└── supervisor.rs        # Signal handling & services keep-alive
```

To add a new built-in service (e.g. `Redis`, `Meilisearch`, `LocalStack`):
1. Create `src/services/<service_name>.rs` containing:
   - Its TOML configuration struct
   - Its `ServiceProvider` implementation (`prompt_init`, `port_offsets`, `contribute_compose`, `contribute_template_vars`, `post_start`)
2. Register the provider in `BUILTIN_SERVICES` in [src/services/mod.rs](src/services/mod.rs) and add its optional field in `ServicesConfig`.
All commands (`init`, `dev`, `env`, etc.) automatically support it without further changes!

Run the tests with `cargo test`.
