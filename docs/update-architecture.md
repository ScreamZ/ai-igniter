# 🔄 Update & Release Architecture

This document describes how `ai-igniter` handles self-updating, release distribution, and non-blocking background notifications.

---

## 1. High-Level Architecture

`ai-igniter` uses a dual-engine strategy:

```
                      ┌─────────────────────────────────┐
                      │        ai-igniter update        │
                      └────────────────┬────────────────┘
                                       │
                    ┌──────────────────┴──────────────────┐
                    ▼                                     ▼
        ┌───────────────────────┐             ┌───────────────────────┐
        │ 1. GitHub Releases    │             │ 2. Cargo / Crates.io  │
        │    (Pre-built Binary) │  (Fallback) │    (From Source)      │
        │    Fast (<2s), No Rust│ ──────────► │    cargo install      │
        │    toolchain required │             │    ai-igniter --force │
        └───────────────────────┘             └───────────────────────┘
```

1. **GitHub Releases (Primary)**:
   - Queries `https://api.github.com/repos/ScreamZ/ai-igniter/releases/latest`.
   - Downloads the pre-built tarball / zip matching the host target triple.
   - Extracts the binary and atomically replaces the running executable via `self-replace`.
2. **Cargo (Fallback & Explicit)**:
   - Triggered when `--cargo` is passed or when GitHub Releases is unavailable (e.g. rate-limiting, custom architecture).
   - Spawns `cargo install ai-igniter --force`.

---

## 2. Target Triples & Asset Naming

The updater resolves target triples defined in [`.github/workflows/release.yml`](../.github/workflows/release.yml):

| Operating System | Architecture | Target Triple | Release Asset Name |
| :--- | :--- | :--- | :--- |
| **macOS** | Apple Silicon (M1/M2/M3/M4) | `aarch64-apple-darwin` | `ai-igniter-aarch64-apple-darwin.tar.gz` |
| **macOS** | Intel x86_64 | `x86_64-apple-darwin` | `ai-igniter-x86_64-apple-darwin.tar.gz` |
| **Linux** | Static musl (Alpine, etc.) | `x86_64-unknown-linux-musl` | `ai-igniter-x86_64-unknown-linux-musl.tar.gz` |
| **Linux** | glibc (Ubuntu, Debian, Fedora) | `x86_64-unknown-linux-gnu` | `ai-igniter-x86_64-unknown-linux-gnu.tar.gz` |
| **Windows** | x86_64 MSVC | `x86_64-pc-windows-msvc` | `ai-igniter-x86_64-pc-windows-msvc.zip` |

---

## 3. Non-Blocking Background Notifications

To notify users without degrading performance, `ai-igniter` implements a non-blocking cached update check:

```
[CLI Command Executed (e.g., ai-igniter status)]
         │
         ├──► Reads ~/.ai-igniter/update_cache.json (< 1ms)
         │    Is last_checked_at older than 24h?
         │      ├── YES ──► Spawns background detached thread to poll GitHub
         │      └── NO  ──► Skip network call completely
         │
         ├──► Command executes normally (dev, env, status, teardown...)
         │
         └──► On Command Exit (Drop of UpdateNotifierGuard):
              If cache indicates a newer version AND stderr is a TTY:
              Prints a non-intrusive banner.
```

### Safety & UX Guarantees:
- **Zero latency overhead**: Main commands never wait for network calls or DNS resolution.
- **24-hour Cache**: Rate-limiting friendly (only 1 check per day per user).
- **Piping safe**: The banner is emitted to `stderr` and only when `stderr.is_terminal()` is `true`. Scripts using `ai-igniter env > .env` will never be corrupted.

### Cache file location & structure:
- **Path**: `~/.ai-igniter/update_cache.json`
```json
{
  "last_checked_at": 1742422800,
  "latest_version": "1.1.0"
}
```

---

## 4. CLI Commands

```bash
# Check and update to the latest version immediately
ai-igniter update

# Check if an update is available without replacing binary
ai-igniter update --check

# Force compilation and update via Cargo
ai-igniter update --cargo
```
