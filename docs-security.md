# Security Notes

- Bind YTDLP Web to a private interface, not `0.0.0.0`.
- Keep `.env`, local TOML, stores, downloads, and logs out of Git.
- Use a PBKDF2-SHA256 hash from `ytdlp-web hash-password --stdin` in
  configuration, not a plaintext password. Argon2 hashes from the Rust version
  are refused at start; the legacy `sha256:SALT_HEX:HASH_HEX` form still works
  but is one fast SHA-256, so replace it.
- Configure a stable `YTDLP_WEB_COOKIE_SECRET` of at least 32 decoded bytes for
  authenticated deployments and share it across instances. Missing or invalid
  values create an ephemeral key that invalidates sessions and OIDC flows on
  restart and is unsuitable for multi-instance operation.
- Managed OIDC is alpha. Keep the local password as break-glass access, use
  HTTPS outside loopback, and do not treat this pilot as enterprise-ready.
- OIDC state is one-time, browser-bound, and short lived. App session tokens
  are opaque and only their HMAC-SHA256 digests are stored in the store.
- OIDC discovery refuses redirects and rejects non-HTTPS endpoints except for
  explicit loopback HTTP dogfood URLs. Token client authentication is selected
  from provider metadata and limited to `client_secret_post` or
  `client_secret_basic`.
- No third-party code: YTDLP Web is built from Go's standard library only
  (`go list -m all` names only YTDLP Web). The OIDC client, the ID token check
  and the store are in this repository. The Rust version's `openidconnect`
  dependency and its `rsa` advisory (RUSTSEC-2023-0071) are gone with it.
- Downloads run `yt-dlp` directly without a shell and only accept YouTube URLs.
  Each run is killed after `YTDLP_WEB_DOWNLOAD_TIMEOUT_SECS`, and stderr is
  drained so a chatty run cannot wedge the queue.
- Rejected passwords and Basic credentials are counted per client address;
  five failures in ten minutes lock that address out for up to ten minutes,
  before any password hashing runs.
- Set `YTDLP_WEB_OIDC_ALLOWED_SUBJECTS` or `YTDLP_WEB_OIDC_ALLOWED_EMAILS` with
  managed login; without them every identity at the issuer is accepted.
- Run `go run . audit-public` before publishing. It checks tracked files
  for ignored private paths, common secret markers, Tailscale-style private
  IPs, private key material, and optional local denylist terms.
- Install local Git hooks with `go run . audit-public --install-hook`.

Host-specific denylist terms can be stored in ignored files:

```text
docs/private/audit-denylist.txt
.ytdlp_web/audit-denylist.txt
```
