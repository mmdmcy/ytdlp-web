# YTDLP Web Architecture

YTDLP Web is a small authenticated download service. The module tree follows
product capabilities and keeps delivery, policy, storage, and external process
code separate.

## Runtime Shape

- One Rust process serves Axum routes, keeps active jobs in memory, uses SQLite
  for the completed-download cache, and launches `yt-dlp` without a shell.
- `src/main.rs` only enters the CLI interface; `src/app/` owns configuration,
  shared state, database opening, and server startup.

## Core Boundaries

| Capability | Owner | Entry points and dependencies |
| --- | --- | --- |
| Process startup and configuration | `src/app/` | Loads `YTDLP_WEB_*`, opens SQLite, assembles shared state, and starts Axum. Depends on persistence and HTTP routing. |
| Download lifecycle | `src/features/downloader/` | Owns jobs, URL/cache identity, retention, and download-cache records. Depends on app state and SQLite. |
| `yt-dlp` execution | `src/integrations/ytdlp.rs` | Owns subprocess arguments, output parsing, and downloaded-file discovery. Called by the download capability. |
| HTTP delivery | `src/interfaces/http/` | Owns routes, request handlers, file responses, component status, and HTML presentation. Depends on app state, downloads, and security. |
| CLI delivery | `src/interfaces/cli.rs` | Dispatches `serve`, `hash-password`, and `audit-public`. Depends on app startup and security commands. |
| Identity and publication safety | `src/security/` | Owns local authentication, OIDC, sessions, and public-source auditing. Depends on app state and HTTP presentation. |
| SQLite schema | `src/persistence/` | Supplies versioned migrations to the app-owned database opener. It does not own download behavior. |

HTTP handlers call feature entry points; the downloader feature calls the
`yt-dlp` integration and its own cache repository. Security guards delivery,
while external processes never own routes or presentation.

## Data Model

- Active jobs and progress logs are process-local state.
- Versioned migrations own the completed-download cache, app sessions, and
  pending OIDC flows.
- The ownership refactor does not change the SQLite schema or migration order.

## Trust, Privacy, And Cost Boundaries

- Authentication, OIDC validation, session hashing, and publication auditing
  remain under `src/security/`.
- File responses remain constrained to the configured canonical download root,
  and submitted URLs remain restricted to supported YouTube hosts.
- The configured semaphore limits concurrent subprocesses; retention policy
  bounds completed jobs and cached files.

## Extension Points

- Add download rules and lifecycle behavior in `src/features/downloader/`.
- Keep external command details in `src/integrations/ytdlp.rs`, SQL schema in
  `src/persistence/`, and delivery behavior in `src/interfaces/`.

## Verification

```bash
cargo fmt --all --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test
cargo run --quiet -- audit-public
cargo katrust inspect
git diff --check
```
