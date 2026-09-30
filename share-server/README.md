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
hangs costs a request nothing, and no volume of forged `kid`s can turn into
outbound requests. The key set is fetched at startup, hourly after that, and
every 5s while fetching is failing — a service that comes up before its
network must recover on its own.

A rotation is therefore picked up within the hour. Cloudflare publishes a new
key before it retires the old one, so tokens keep verifying across the change.

Which refusal a caller gets turns on whether the key set is **current**, not
on whether the last refresh happened to fail:

| key set | `kid` we hold | `kid` we do not |
|---|---|---|
| fetched within the hour | verify | 401 — we hold what the team publishes |
| fetched longer ago | verify, for a day | 503 — we could not check |
| never fetched | — | 503 |

An outage must not read as a wall of user auth failures, and a forged flood
during one must not read as an outage.

**Switching an existing deployment from `forwarded_header` changes who owns
what.** The principal id is derived from the Access identity rather than from
the header value, so transcripts published under the old scheme keep their old
owner prefix: still readable, no longer writable by the person who published
them. Migrate the keys, or start on an empty prefix.

### Setting it up

[Cloudflare Access in front of the share service](../docs/cloudflare-access.md)
is the end-to-end guide: the Zero Trust side (team, application, AUD tag,
service tokens), the service and NixOS configuration, the checks to run once
against a real tenant — no local test can cover those — and what each failure
looks like in the log.

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
suite cannot state: a rotation picked up by the refresher, a flood of forged
`kid`s that costs no fetch at all, a key set that keeps working when
refreshing it starts failing, and a hung certs endpoint that costs the request
path nothing.
