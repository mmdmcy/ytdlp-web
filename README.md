# YTDLP Web

YTDLP Web is a private Rust web queue for downloading YouTube videos through
`yt-dlp`.

It is intended for localhost, LAN, VPN, or tailnet use. Do not expose it
directly to the public internet.

## Features

- Submit YouTube URLs through a small web form.
- One active download by default.
- Progress polling while a job runs.
- Download cache backed by SQLite.
- File serving constrained to the configured download directory.

## Run

```sh
cargo run -- hash-password --stdin
YTDLP_WEB_PASSWORD_HASH='$argon2id$...' cargo run -- serve
```

Default bind: `127.0.0.1:8790`.

For local-only development without auth:

```sh
YTDLP_WEB_AUTH_DISABLED=1 cargo run -- serve
```

## Configuration

```text
YTDLP_WEB_BIND=127.0.0.1:8790
YTDLP_WEB_DB=data/ytdlp_web.sqlite
YTDLP_WEB_DOWNLOAD_DIR=data/downloads
YTDLP_WEB_YTDLP=yt-dlp
YTDLP_WEB_MAX_ACTIVE=1
YTDLP_WEB_JOB_TTL_HOURS=24
YTDLP_WEB_PASSWORD_HASH=<argon2 hash>
YTDLP_WEB_COOKIE_SECRET=<random hex>
```

Keep real `.env` files, downloaded media, databases, logs, and host-specific
deployment state out of this repository.

## Checks

```sh
cargo fmt --check
cargo test
cargo run -- audit-public
```
