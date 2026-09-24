# txcript-share-server

The native HTTP host for the shared transcript service. One static binary,
one configuration file.

It does I/O and nothing else: every decision comes from
`txcript_share_core::decide`, the same function the Cloudflare Worker calls
through wasm. There is no authorization branch in this crate.

## Run

```sh
txcript-share-server /etc/txcript-share/config.toml
```

```toml
listen = "127.0.0.1:8787"          # loopback by default; put a proxy in front

[identity]
kind = "static_tokens"             # or "forwarded_header", "cloudflare_access"
header = "x-token"
tokens_file = "/run/credentials/txcript-share.service/tokens"

[store]
kind = "filesystem"                # or "memory", or "s3" (feature `s3`)
root = "/var/lib/txcript-share"

[policy]
kind = "owner_prefix"              # or "read_only_mirror", "team_scoped"

[limits]
max_document_bytes = 10485760
page_size = 1000
```

## Routes

| | |
|---|---|
| `PUT /s/<session-id>` | publish, under your own prefix |
| `GET /s/<owner>/<session-id>` | read |
| `DELETE /s/<owner>/<session-id>` | delete your own |
| `GET /s?owner=me` | list |

A `PUT` carries only a bare session id. The owner segment comes from the
authenticated principal, so writing outside your own namespace is not
*expressible* rather than merely denied.

### S3 and compatible backends

Built with `--features s3`, off by default so a filesystem deployment does
not compile, ship, or audit the AWS client tree.

```toml
[store]
kind = "s3"
bucket = "txcript-share"
endpoint = "https://<account>.r2.cloudflarestorage.com"  # omit for AWS
force_path_style = true                                   # MinIO, Ceph
```

Credentials are **not** configured here. They come from the ambient AWS
chain — instance role, web identity, or a credentials file the deployment
mounts — so the same binary works under an IAM role, a Kubernetes service
account, and a static key file without knowing which it is.

## Configuration is the deployment seam

Secrets arrive as **file paths**, never environment values, so systemd
`LoadCredential`, Kubernetes projected secrets, ECS secrets, sops, and agenix
all work without this binary knowing any of them exist. The moment it reads a
credential out of the environment itself, it is coupled to one delivery
mechanism and several deployment targets are lost.

Unknown configuration keys are an error rather than a default: a typo in a
security-relevant field must not read as "off".

## Identity

- `static_tokens` — a token table from a file, one `<token> <id> [label]` per
  line. Real for a closed set of machine clients, and the double every other
  seam's tests run against.
- `forwarded_header` — trust an identity header set by an authenticating
  proxy. **The proxy must be the only route to this service.** If the origin
  is reachable directly, anyone can set the header and become anyone. Bind to
  loopback, or use a network policy.
- `cloudflare_access` — verify `Cf-Access-Jwt-Assertion` here, against the
  team's JWKS and the application's AUD tag. Access in front is the gate,
  this is the lock behind it, so a direct route to the origin is no longer a
  forgery hole.

```toml
[identity]
kind = "cloudflare_access"
team = "example"                   # the <team> of <team>.cloudflareaccess.com
aud_file = "/run/credentials/txcript-share.service/access-aud"
```

Built with `--features cloudflare_access`, off by default for the same reason
`s3` is: a deployment that authenticates some other way should not compile or
ship a TLS client it never calls. Under NixOS,
`nixosModules.cloudflare-access` selects both the identity and the matching
build.

An SSO login authenticates as its `email`, a service token as its
`common_name`, and the principal id is the SHA-256 of that string — the same
digest `deploy/cloudflare/src/support.js` computes, so the Worker and this
host agree on who owns a transcript when both front one bucket.

Fetching happens on a refresher thread, never on a request: `principal()`
reads the key set that thread published and returns, so a certs endpoint that
hangs costs a request nothing. The key set is fetched once at startup, on the
hour after that, and whenever a request reports a `kid` the published set does
not have — which is how a key rotation recovers in the moment rather than an
hour later. That report is a doorbell, not a fetch, and the refresher keeps a
minute between fetches, so a forged header carrying a random `kid` cannot turn
into an outbound request each. A key set that has never been fetched is a 503,
not a 401: an outage must not read as a wall of user auth failures, and one
already held keeps verifying honest tokens while refreshing it fails.

**Switching an existing deployment from `forwarded_header` changes who owns
what.** The principal id is derived from the Access identity rather than from
the header value, so transcripts published under the old scheme keep their old
owner prefix: still readable, no longer writable by the person who published
them. Migrate the keys, or start on an empty prefix.

### Verifying it against a real tenant

Local tests cover the token handling with a key set of their own. What they
cannot cover is Cloudflare, so once per deployment:

- an unauthenticated browser request gets a **302 to the SSO login**, not a
  401 — that is Access in front doing its job;
- a human SSO login publishes, and the owner segment of the returned slug is
  stable across logins;
- a service token (`CF-Access-Client-Id` / `CF-Access-Client-Secret`)
  authenticates and lands on a *different* owner segment from the human;
- a request carrying a hand-written `Cf-Access-Jwt-Assertion`, sent straight
  to the origin, is refused.

## Testing

```sh
cargo test -p txcript-share-server --all-features
```

`tests/access_over_http.rs` runs the access matrix over real HTTP against a
real filesystem store. The security cases do not stop at the status code:
they re-read the object afterwards and assert the bytes are unchanged, because
a 403 that still mutates the store is the failure a status-only assertion
misses — and that is exactly what a sabotaged handler produced when this was
checked.

`src/access.rs` mints real RS256 tokens from a throwaway key and runs the
shared `identity::conformance` suite against the verifier, plus the cases a
suite cannot state: a rotation recovered from without waiting out the TTL, a
flood of forged `kid`s that costs one fetch, a key set that keeps working when
refreshing it starts failing, and a hung certs endpoint that costs the request
path nothing.
