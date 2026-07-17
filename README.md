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
- Local Argon2 password login with an optional OpenID Connect organization
  login using Authorization Code, S256 PKCE, state, and nonce verification.

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
YTDLP_WEB_AUTH_DISABLED=0
YTDLP_WEB_PASSWORD_HASH=<argon2 hash>
YTDLP_WEB_COOKIE_SECRET=<stable random hex, at least 32 bytes>
YTDLP_WEB_COOKIE_SECURE=0
```

Set `YTDLP_WEB_COOKIE_SECRET` explicitly whenever authentication is enabled.
Keep the same deployment-managed value across restarts and all instances; a
64-character value from `openssl rand -hex 32` is sufficient. If the value is
absent, invalid hex, or shorter than 32 decoded bytes, YTDLP Web generates an
ephemeral process-local key. That development fallback invalidates sessions
and pending OIDC flows on restart and cannot support multiple instances.

YTDLP Web remains fully standalone. Managed login is an optional dogfood-alpha
integration, not an enterprise-readiness or certification claim. To enable it,
set all four values below and keep `YTDLP_WEB_PASSWORD_HASH` configured as an
independent local break-glass path:

```text
YTDLP_WEB_OIDC_ISSUER=https://identity.example.invalid
YTDLP_WEB_OIDC_CLIENT_ID=ytdlp-web
YTDLP_WEB_OIDC_CLIENT_SECRET=<oidc client secret>
YTDLP_WEB_OIDC_REDIRECT_URL=https://ytdlp-web.example.invalid/auth/oidc/callback
```

The issuer and redirect URL must use HTTPS. Loopback HTTP is accepted only for
explicit local dogfood testing. Discovered authorization, token, userinfo, and
JWKS endpoints follow the same rule, and discovery redirects are refused. The
client prefers advertised `client_secret_post`, supports
`client_secret_basic`, uses the standards default of `client_secret_basic` when
the discovery field is absent, and rejects providers that advertise neither.
Provider access, refresh, and ID tokens are not stored after the callback. If
the provider is unavailable, organization login can fail without disabling the
local login form.

For local service discovery and health checks, YTDLP Web exposes a minimal,
unauthenticated status envelope at
`/.well-known/linuxmice/component`. It contains no job URLs, filenames, media
paths, database paths, credentials, identity endpoints, or user data. The
application remains fully standalone. Its `identity_mode` is `disabled`,
`local`, or `oidc+local` according to the active configuration.
The matching optional catalog declaration is `linuxmice-component.toml`;
YTDLP Web does not require the LinuxMice hub or identity service to run.

Keep real `.env` files, downloaded media, databases, logs, and host-specific
deployment state out of this repository.

## Checks

```sh
cargo fmt --check
cargo test
cargo run -- audit-public
```
