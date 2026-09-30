# Deploying a share service

A share service stores transcripts so a group can publish their own and read
everyone's: `txcript push` writes one, `txcript pull` fetches one, and
`txcript list` includes what others published.

There are two hosts and **one** set of rules. Who may do what is decided by
`share-core`, a pure function with no I/O; each host only carries out what it
returns. Pick a host on operational grounds — the authorization behaviour is
the same either way.

| | Cloudflare Worker + R2 | Self-hosted binary |
|---|---|---|
| you run | nothing | a host, or a container |
| identity | Cloudflare Access (built in) | Access, a proxy header, or static tokens |
| storage | R2 | filesystem, or any S3-compatible bucket |
| start here if | you want it working today | you need your own storage or identity |

> **Read this before you deploy.** Transcripts carry working-directory paths,
> branch names, and tool output. A service with nothing authenticating in
> front of it publishes all of that to anyone who finds the URL — HTTPS does
> not change that. Both paths below put an identity gate in front; do not
> remove it.

---

## Path 1 — Cloudflare Worker + R2

### What you need

- A Cloudflare account with R2 enabled and Zero Trust set up
- The dev shell, which carries `wrangler`, `node`, and a `wasm-bindgen` CLI
  matching the pinned crate: `nix develop .#js`

  Without Nix: Node 24+, `wrangler`, the `wasm32-unknown-unknown` Rust
  target, and `wasm-bindgen-cli` **exactly** version 0.2.126. A mismatched
  `wasm-bindgen` produces bindings that fail at runtime rather than failing
  the build.

### 1. Create the bucket

```sh
wrangler login
wrangler r2 bucket create txcript-share
```

The name must match `bucket_name` in `deploy/cloudflare/wrangler.toml`.

### 2. Create the Access application, and copy its AUD tag

In the Zero Trust dashboard:

1. **Access → Applications → Add an application → Self-hosted**
2. Set the domain to the hostname the Worker will serve — either
   `txcript-share.<your-subdomain>.workers.dev` or a custom domain you route
   to the Worker
3. Add a policy for who gets in (for example **Include → Emails ending in**
   `@your-company.com`)
4. Open the application's **Overview** and copy the **Application Audience
   (AUD) Tag**

Your team name is the `<team>` in `<team>.cloudflareaccess.com`.

For more on the Zero Trust side — policies, service tokens, what each failure
looks like in the log, and the checks no local test can cover — see
[Cloudflare Access in front of the share service](cloudflare-access.md).

### 3. Configure and deploy

Edit `deploy/cloudflare/wrangler.toml`:

```toml
[vars]
ACCESS_TEAM = "your-team"          # <team>.cloudflareaccess.com
ACCESS_AUD  = "the-aud-tag-you-just-copied"
POLICY      = "owner_prefix"       # see Policies below
```

Neither value is a credential — they name your tenant and application — so
they belong in this file rather than in `wrangler secret`.

```sh
cd deploy/cloudflare
npm test          # the I/O half, under node --test
npm run deploy    # builds the Rust core to wasm, then wrangler deploy
```

If your account has more than one zone, set `CLOUDFLARE_ACCOUNT_ID` or add
`account_id` to `wrangler.toml`.

### 4. Create a service token for the CLI

A browser login gets a human through Access; a CLI needs a service token.

1. **Access → Service Auth → Create Service Token**; save the Client ID and
   Client Secret (the secret is shown once)
2. Add a policy on the application: **Include → Service Token →** the one you
   just made

Then go to [Point a client at it](#point-a-client-at-it).

---

## Path 2 — self-hosted binary

One static binary, one configuration file, no arguments beyond its path.

### 1. Build it

Storage backends and identity providers are compile-time features, so a
deployment does not ship or audit code it never calls.

```sh
nix build .#share-server              # filesystem storage, header identity
nix build .#share-server-s3           # + the S3 backend
nix build .#share-server-access       # + Cloudflare Access JWT verification
nix build .#share-server-s3-access    # both
nix build .#container                 # OCI image, docker load -i
```

The container wraps the base `share-server` — filesystem storage, header
identity — and holds only the binary's closure plus CA certificates: no base
image and no distro to track CVEs for. For S3 or Access in a container, point
`nix/container.nix` at the variant you need.

From a clone without Nix:

```sh
cargo install --path share-server --features s3,cloudflare_access
```

### 2. Write the configuration

Unknown keys are a startup error, never a silently ignored default — a typo
in a security-relevant field must not read as "off".

```toml
listen = "127.0.0.1:8787"       # loopback; put an authenticating proxy in front

[identity]
kind = "static_tokens"          # or "cloudflare_access", or "forwarded_header"
header = "x-token"
tokens_file = "/etc/txcript-share/tokens"

[store]
kind = "filesystem"             # or "s3", or "memory"
root = "/var/lib/txcript-share"

[policy]
kind = "owner_prefix"

[limits]
max_document_bytes = 10485760
page_size = 1000
```

**Identity.** Pick deliberately:

- `static_tokens` — a table of `<token> <principal-id> [label]` lines,
  `chmod 600`. Good for a closed set of machines.
- `cloudflare_access` — verifies `Cf-Access-Jwt-Assertion` against the team's
  JWKS and the AUD tag, at the origin. A request arriving by another route
  still cannot forge an identity. Needs `--features cloudflare_access`:

  ```toml
  [identity]
  kind = "cloudflare_access"
  team = "your-team"
  aud_file = "/run/credentials/txcript-share.service/access-aud"
  ```

- `forwarded_header` — trusts an identity header written by a proxy. **Only
  safe while that proxy is the single route to the service.** If the origin
  is reachable directly, anyone can set the header and become anyone. Bind to
  loopback, or use a network policy.

**S3 storage.** Credentials are deliberately *not* configured here; they come
from the ambient AWS chain, so an instance role, a web identity, and a key
file all work without the binary knowing which:

```toml
[store]
kind = "s3"
bucket = "my-txcript-bucket"
endpoint = "https://<account>.r2.cloudflarestorage.com"  # omit for AWS
root = "prod"                                            # optional prefix
force_path_style = true                                  # R2, MinIO, Ceph
```

`AWS_REGION` must be set. The service needs one identity with read and write
over the bucket; publishers do not need bucket credentials of their own.

### 3. Run it

```sh
AWS_REGION=us-east-1 txcript-share-server /etc/txcript-share/config.toml
```

### On NixOS

The service module and the authentication module are separate on purpose:
changing how callers are identified must not rebuild or reconfigure the
service.

```nix
{
  imports = [
    txcript.nixosModules.txcript-share
    txcript.nixosModules.cloudflare-access
  ];

  services.txcript-share = {
    enable = true;
    store = { kind = "filesystem"; root = "/var/lib/txcript-share"; };
    policy.kind = "owner_prefix";
  };

  services.txcript-share.cloudflareAccess = {
    enable = true;
    tunnelCredentialsFile = config.age.secrets.tunnel.path;
  };
}
```

The module generates the TOML, runs the unit under `DynamicUser` with
`ProtectSystem=strict`, and passes secrets through systemd `LoadCredential`
as **file paths** — which is what lets sops, agenix, and plain files all work
without the binary knowing the difference. `cloudflared` runs as its own
unit and is the only ingress.

The Zero Trust side, end to end, is
[Cloudflare Access in front of the share service](cloudflare-access.md).

---

## Point a client at it

Release binaries include the share client. Configuring an endpoint is what
opts a machine in: with nothing set, txcript contacts nobody.

```sh
export TXCRIPT_SHARE_URL=https://share.example.com
```

Credentials are headers, written the way you would in a shell. Any variable
beginning `TXCRIPT_SHARE_HEADER` is sent, so a Cloudflare Access service
token — which is two headers — needs no code:

```sh
export TXCRIPT_SHARE_HEADER_ID="CF-Access-Client-Id: <client-id>"
export TXCRIPT_SHARE_HEADER_SECRET="CF-Access-Client-Secret: <client-secret>"
```

For a `static_tokens` service it is one header:

```sh
export TXCRIPT_SHARE_HEADER_TOKEN="x-token: s3cr3t-alice"
```

Then:

```sh
txcript push <session-id>        # publish; prints the slug
txcript list --from share        # what everyone published
txcript pull <slug>              # fetch as a Simple document
txcript pull <slug> --with codex # or as a resumable Codex session
```

### Verify a deployment

```sh
curl -s -X PUT -H "x-token: s3cr3t-alice" \
  --data-binary @doc.json https://share.example.com/s/sess-1     # 201 + slug
curl -s -H "x-token: s3cr3t-alice" https://share.example.com/s   # 200 + listing
curl -s https://share.example.com/s                              # 401
```

The third is the one that matters: an unauthenticated request must be
refused. Against Cloudflare Access in a browser you get a redirect to SSO
instead of a 401 — that is Access doing its job in front.

### Publishing straight to a bucket, with no service

If everyone already has bucket credentials and "write your own prefix, read
everything" is the whole rule, you can skip the service.

Create a bucket with no public access — its own policy is now the entire
access boundary. Then give **each publisher their own prefix**, because this
is what makes ownership real: without the prefix condition any publisher can
overwrite any other, and the client neither enforces that nor can.

```json
{
  "Version": "2012-10-17",
  "Statement": [
    { "Sid": "ReadEveryTranscript", "Effect": "Allow",
      "Action": ["s3:GetObject"],
      "Resource": "arn:aws:s3:::my-txcript-bucket/*" },
    { "Sid": "ListTheBucket", "Effect": "Allow",
      "Action": ["s3:ListBucket"],
      "Resource": "arn:aws:s3:::my-txcript-bucket" },
    { "Sid": "WriteOnlyMyOwnPrefix", "Effect": "Allow",
      "Action": ["s3:PutObject", "s3:DeleteObject"],
      "Resource": "arn:aws:s3:::my-txcript-bucket/alice/*" }
  ]
}
```

```sh
cargo install --path cli --features share_s3
export TXCRIPT_SHARE_BUCKET=my-txcript-bucket
export TXCRIPT_SHARE_OWNER=alice          # must match the prefix IAM allows
export TXCRIPT_SHARE_ENDPOINT=…           # optional: R2, MinIO, Ceph
```

`alice` must match the prefix in the policy, or writes fail on the first
publish rather than at configuration time.

What you give up: every reader needs bucket credentials, nothing validates
that what lands is a transcript, policies a prefix cannot express (team
scoping) are unavailable, and listing costs one `HEAD` per entry. Both paths
write an identical bucket layout, so a service placed in front of a bucket
written directly lists it correctly, and vice versa.

Setting both `TXCRIPT_SHARE_URL` and `TXCRIPT_SHARE_BUCKET` is an error
rather than a silent preference.

---

## Policies

| `POLICY` / `[policy] kind` | rule |
|---|---|
| `owner_prefix` (default) | read everyone's, write only your own |
| `read_only_mirror` | everyone reads, nobody writes |
| `team_scoped` | read within your team, write only your own |
| `allow_all` | everything — for tests, never for a deployment |

No policy, including `allow_all`, can produce a cross-owner write: `PUT`
takes a bare session id and the owner segment comes from the authenticated
principal, so writing outside your own namespace is not expressible rather
than merely denied.

`team_scoped` needs `<principal-id>:<team>` pairs. A principal id is the
SHA-256 of the Access identity — an email for an SSO login, the
`common_name` for a service token:

```sh
printf %s alice@example.com | sha256sum
```

Both hosts compute it the same way, so a Worker and a self-hosted service can
front one bucket and agree on who owns what.

## Limits

Documents are capped at 10 MB and listings page at 1000 entries. On the
self-hosted host both are configurable under `[limits]`; in the Worker they
are constants in `deploy/cloudflare/src/index.js`.
