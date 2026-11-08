# YTDLP Web

YTDLP Web is a private web queue for downloading YouTube videos through
`yt-dlp`. It is one Go program built from the standard library only: no
third-party modules, no database library and no cgo.

It is intended for localhost, LAN, VPN, or tailnet use. Do not expose it
directly to the public internet.

## Features

- Submit YouTube URLs through a small web form.
- One active download by default.
- Progress polling while a job runs.
- Download cache kept in a checksummed JSON file (see Storage).
- File serving constrained to the configured download directory.
- Local PBKDF2 password login with an optional OpenID Connect organization
  login using Authorization Code, S256 PKCE, state, and nonce verification.

## Run

```sh
go build -o ytdlp-web .
printf '%s' 'the password' | ./ytdlp-web hash-password --stdin
YTDLP_WEB_PASSWORD_HASH='$pbkdf2-sha256$...' ./ytdlp-web serve
```

Default bind: `127.0.0.1:8790`.

For local-only development without auth:

```sh
YTDLP_WEB_AUTH_DISABLED=1 ./ytdlp-web serve
```

## Configuration

```text
YTDLP_WEB_BIND=127.0.0.1:8790
YTDLP_WEB_DB=data/ytdlp_web.sqlite
YTDLP_WEB_DOWNLOAD_DIR=data/downloads
YTDLP_WEB_YTDLP=yt-dlp
YTDLP_WEB_JS_RUNTIME=<optional path to node for --js-runtimes>
YTDLP_WEB_MAX_ACTIVE=1
YTDLP_WEB_JOB_TTL_HOURS=24
YTDLP_WEB_DOWNLOAD_TIMEOUT_SECS=7200
YTDLP_WEB_AUTH_DISABLED=0
YTDLP_WEB_PASSWORD_HASH=<hash from hash-password>
YTDLP_WEB_COOKIE_SECRET=<stable random hex, at least 32 bytes>
YTDLP_WEB_COOKIE_SECURE=0
YTDLP_WEB_USER=ytdlp_web
```

`YTDLP_WEB_DB` keeps the name it had when YTDLP Web used SQLite: the store is
the same path with `.json` (`data/ytdlp_web.sqlite` means
`data/ytdlp_web.json`). A path ending in `.json` names the store directly.

`hash-password` makes PBKDF2-SHA256 hashes
(`$pbkdf2-sha256$i=600000,l=32$SALT$HASH`; 600,000 iterations is OWASP's
recommendation for PBKDF2-HMAC-SHA256). The legacy `sha256:SALT_HEX:HASH_HEX`
form is still checked, as before, but is one fast SHA-256: replace it with a
new hash. The Argon2 hashes of the Rust version cannot be checked without an
Argon2 implementation, which Go's standard library does not have: YTDLP Web
refuses to start with one and names the command that makes a new hash.
`YTDLP_WEB_USER` is the user name for HTTP Basic credentials.

A `yt-dlp` run that is still going after `YTDLP_WEB_DOWNLOAD_TIMEOUT_SECS`
(default two hours) is killed and the job fails, freeing its download slot.
The process's stderr is drained while it runs and its last few KiB are shown
in the job error. After five rejected passwords or Basic credentials from one
client address within ten minutes, further attempts from that address are
refused with `429` until the oldest failure is ten minutes old.

Set `YTDLP_WEB_COOKIE_SECRET` explicitly whenever authentication is enabled.
Keep the same deployment-managed value across restarts and all instances; a
64-character value from `openssl rand -hex 32` is sufficient. If the value is
absent, invalid hex, or shorter than 32 decoded bytes, YTDLP Web generates an
ephemeral process-local key. That development fallback invalidates sessions
and pending OIDC flows on restart.

YTDLP Web remains fully standalone. Managed login is an optional dogfood-alpha
integration, not an enterprise-readiness or certification claim. To enable it,
set all four values below and keep `YTDLP_WEB_PASSWORD_HASH` configured as an
independent local break-glass path:

```text
YTDLP_WEB_OIDC_ISSUER=https://identity.example.invalid
YTDLP_WEB_OIDC_CLIENT_ID=ytdlp-web
YTDLP_WEB_OIDC_CLIENT_SECRET=<oidc client secret>
YTDLP_WEB_OIDC_REDIRECT_URL=https://ytdlp-web.example.invalid/auth/oidc/callback
YTDLP_WEB_OIDC_ALLOWED_SUBJECTS=<comma-separated sub claims>
YTDLP_WEB_OIDC_ALLOWED_EMAILS=<comma-separated email claims>
```

A verified ID token only creates a session when its `sub` is listed in
`YTDLP_WEB_OIDC_ALLOWED_SUBJECTS` or its `email` (case-insensitive) is listed
in `YTDLP_WEB_OIDC_ALLOWED_EMAILS`; other identities get `403` and a log line
with the subject. When neither list is set, every identity the issuer vouches
for is accepted and YTDLP Web prints a startup warning saying so. Email
matching trusts the issuer's `email` claim, so prefer subjects where the
provider allows users to change their address.

The issuer and redirect URL must use HTTPS. Loopback HTTP is accepted only for
explicit local dogfood testing. Discovered authorization, token, userinfo, and
JWKS endpoints follow the same rule, and discovery redirects are refused. The
client prefers advertised `client_secret_post`, supports
`client_secret_basic`, uses the standards default of `client_secret_basic` when
the discovery field is absent, and rejects providers that advertise neither.
ID tokens signed with EdDSA (Ed25519), RS256, ES256 or HS256 are checked
against the provider's advertised algorithms and keys. Provider access,
refresh, and ID tokens are not stored after the callback. If the provider is
unavailable, organization login can fail without disabling the local login
form.

For local service discovery and health checks, YTDLP Web exposes a minimal,
unauthenticated status envelope at
`/.well-known/linuxmice/component`. It contains no job URLs, filenames, media
paths, database paths, credentials, identity endpoints, or user data. The
application remains fully standalone. Its `identity_mode` is `disabled`,
`local`, or `oidc+local` according to the active configuration.
The matching optional catalog declaration is `linuxmice-component.toml`;
YTDLP Web does not require the LinuxMice hub or identity service to run.

Keep real `.env` files, downloaded media, stores, logs, and host-specific
deployment state out of this repository.

## Storage

- **One file**, `ytdlp_web.json`: the download cache, app sessions and pending
  OIDC sign-ins. Jobs live in memory, as before.
- **Refused, never repaired:** the first line names the format and the SHA-256
  of the rest; a file whose checksum, JSON or rules do not hold is refused.
- **Atomic writes:** a change writes a new owner-only file beside the old one,
  syncs it, renames it over the old one and syncs the folder.
- **One process:** YTDLP Web holds an exclusive lock on `ytdlp_web.json.lock`
  while it runs.

### Moving from the Rust version

`import-sqlite` reads a copy of the Rust version's SQLite database once,
through the system's `sqlite3` command (read-only, immutable, schema 2), and
writes the store where `YTDLP_WEB_DB` points. The download cache comes over;
sessions stay behind, so people sign in again. It never overwrites a store and
prints only counts.

```sh
sqlite3 -readonly data/ytdlp_web.sqlite ".backup /tmp/ytdlp-copy.sqlite"
YTDLP_WEB_DB=data/ytdlp_web.sqlite ./ytdlp-web import-sqlite --from /tmp/ytdlp-copy.sqlite
```

The Rust version's Argon2 password hashes are not accepted (see
Configuration): make a new one with `hash-password --stdin`.

The Rust version is kept, buildable, in `archive/rust/`.

## Checks

```sh
gofmt -l .
go vet ./...
go test ./...
go run . audit-public
go list -m all   # only github.com/mmdmcy/ytdlp-web: no third-party modules
```
