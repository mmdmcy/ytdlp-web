# YTDLP Web Architecture

YTDLP Web is a small authenticated download service: one Go program in one
package, built from the standard library only. `main.go` selects a CLI command;
each file owns one capability.

## Runtime Shape

- One Go process serves HTTP with `net/http`, keeps active jobs in memory,
  keeps the completed-download cache and sign-in state in a standard-library
  file store, and launches `yt-dlp` without a shell.

## Core Boundaries

| Capability | File | Entry points and dependencies |
| --- | --- | --- |
| Process startup and configuration | `main.go` | Loads `YTDLP_WEB_*`, opens the store, assembles routes and starts the server. |
| Download lifecycle | `downloads.go` | Jobs, URL and cache identity, retention, cache records and the download handlers. Uses the store and `ytdlp.go`. |
| `yt-dlp` execution | `ytdlp.go` | Subprocess arguments, output parsing, the time limit and downloaded-file discovery. |
| HTTP delivery | `web.go` | Routing, responses, form and query parsing, answered as axum did. |
| HTML presentation | `pages.go`, `page.css` | The download, job and login pages. |
| Durable storage | `store.go` | The checksummed JSON store: download cache, sessions, pending sign-ins. |
| One-time import | `importer.go` | `import-sqlite` through the system's `sqlite3`. |
| Identity | `auth.go`, `password.go`, `oidc.go` | Local authentication, sessions, the login throttle, OIDC sign-in. |
| Status envelope and publication safety | `status.go`, `audit.go` | `/.well-known/linuxmice/component`, `audit-public`. |

## Data Model

- Active jobs and progress logs are process-local state.
- `storeData` holds the completed-download cache, app sessions and pending
  OIDC flows; `validate` enforces the SQL schema's keys and checks.

## Trust, Privacy, And Cost Boundaries

- File responses stay inside the configured canonical download root (whole
  path components), and submitted URLs stay restricted to YouTube hosts.
- The download slots limit concurrent subprocesses; retention bounds completed
  jobs and cached files.

## Verification

```bash
gofmt -l .
go vet ./...
go test ./...
go run . audit-public
go list -m all
git diff --check
```
