# Sharing transcripts

Publish your own sessions, read everyone's, modify only your own.

There are **two ways to run this**, and they write the same bucket. Start
with the simpler one; add the other when you need what it gives you. A bucket
written one way lists and loads correctly through the other, so moving
between them is a configuration change rather than a migration.

| | direct to S3 | behind a service |
|---|---|---|
| to run | nothing | one binary |
| who can read | anyone with bucket credentials | anyone the service authenticates |
| identity | IAM | Access, OIDC, tokens, mTLS — whatever you put in front |
| ownership enforced by | an IAM prefix condition | the service |
| policies | write-your-prefix, read-all | that, plus read-only and team scoping |
| content validated | no | yes — non-transcripts are refused |
| listing cost | one `HEAD` per entry, from your machine | one `HEAD` per entry, inside the service's network |

---

## Path 1 — direct to S3, no service

Best when everyone already has credentials for the bucket and the rule is
"write your own prefix, read everything".

### 1. A bucket

Create one. Nothing public: the bucket's own policy is the access boundary,
so a public-read bucket publishes every transcript to the internet.

### 2. An IAM policy per publisher

This is what makes ownership real. Without the prefix condition, any
publisher can overwrite any other — the client does not enforce it and
cannot.

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "ReadEveryTranscript",
      "Effect": "Allow",
      "Action": ["s3:GetObject"],
      "Resource": "arn:aws:s3:::my-txcript-bucket/*"
    },
    {
      "Sid": "ListTheBucket",
      "Effect": "Allow",
      "Action": ["s3:ListBucket"],
      "Resource": "arn:aws:s3:::my-txcript-bucket"
    },
    {
      "Sid": "WriteOnlyMyOwnPrefix",
      "Effect": "Allow",
      "Action": ["s3:PutObject", "s3:DeleteObject"],
      "Resource": "arn:aws:s3:::my-txcript-bucket/alice/*"
    }
  ]
}
```

Give each publisher their own prefix. `alice` here must match the `owner` the
client is configured with, or writes fail on the first publish.

### 3. Use it

```rust
use txcript::harness::share_s3::DirectStore;
use txcript::Store;

let client = aws_sdk_s3::Client::new(&aws_config::load_from_env().await);
let store = DirectStore::new(
    txcript_share_store::S3::new(client, "my-txcript-bucket"),
    "alice",
)?;

store.save(&transcript)?;        // writes alice/<session-id>
let found = store.discover()?;   // everyone's
```

Build with `--features share_s3`. Credentials come from the ambient AWS
chain, so an instance role, a web identity, or `~/.aws/credentials` all work
without configuration here.

### What you are giving up

- **Everyone needs bucket credentials.** There is no way to let someone read
  without giving them IAM access to the bucket.
- **Only prefix ownership is expressible.** Team scoping reads an object's
  stored metadata, which a bucket policy cannot.
- **Nothing validates what lands.** Anything writable to the prefix goes in
  the bucket, transcript or not.
- **Listing is chatty from your machine.** `ListObjectsV2` does not return
  user metadata, so a listing is one `HEAD` per entry across the internet.

---

## Path 2 — behind a service

Best when readers should not need bucket credentials, when you want identity
from somewhere other than IAM, or when you want a policy a prefix cannot
express.

### 1. Build

```sh
nix build .#share-server-s3        # S3 backend compiled in
# or: cargo build -p txcript-share-server --features s3 --release
```

The default package has no S3 support; the `s3` feature is off so a
filesystem deployment does not ship the AWS client.

### 2. A tokens file

```
# <token> <principal-id> [label]
s3cr3t-alice  alice  alice@example.com
s3cr3t-bob    bob    bob@example.com
```

`chmod 600`. The principal id becomes the key prefix, so keep it a plain
segment.

### 3. Configure

```toml
listen = "127.0.0.1:8787"

[identity]
kind = "static_tokens"        # or "forwarded_header", behind a proxy
header = "x-token"
tokens_file = "/etc/txcript-share/tokens"

[store]
kind = "s3"
bucket = "my-txcript-bucket"
# endpoint + force_path_style are for R2, MinIO, Ceph — omit for AWS

[policy]
kind = "owner_prefix"         # or "read_only_mirror", "team_scoped"
```

Unknown keys are a startup error rather than a default, so a typo in a
security-relevant field cannot read as "off".

### 4. Run

```sh
AWS_REGION=us-east-1 txcript-share-server /etc/txcript-share/config.toml
```

The service needs one IAM identity with read and write over the whole bucket;
publishers no longer need their own. `AWS_REGION` is required.

On NixOS, `nixosModules.txcript-share` generates the config, runs the unit
under `DynamicUser` with `ProtectSystem=strict`, and takes secrets through
systemd credentials. `nixosModules.cloudflare-access` puts a tunnel in front
as a separate import.

### 5. Point the client at it

```sh
export TXCRIPT_SHARE_URL=https://share.example.com
export TXCRIPT_SHARE_HEADER_TOKEN="x-token: s3cr3t-alice"
```

```rust
use txcript::harness::share::ShareStore;
let store = ShareStore::from_env()?;
```

Build with `--features share`. Credentials are a header map, so a Cloudflare
Access service token is two `TXCRIPT_SHARE_HEADER*` variables and needs no
code:

```sh
export TXCRIPT_SHARE_HEADER_ID="CF-Access-Client-Id: ....access"
export TXCRIPT_SHARE_HEADER_SECRET="CF-Access-Client-Secret: ..."
```

---

## Security, either way

**The store is not the boundary; the thing in front of it is.** Direct to S3
that is the bucket policy. Behind a service it is whatever authenticates —
and if the service is reachable without authentication, every transcript is
public over HTTPS.

Transcripts carry working directory paths, branch names, source code, and
tool output. Treat a share bucket as you would a private source repository.

## Moving between them

A bucket written directly reads correctly through a service placed in front
of it later, and the reverse. Both write the same keys
(`<owner>/<session-id>`) and the same metadata, from one shared projection,
and `tests/integration/share_round_trip.rs` asserts both directions against a
live S3.

To move from direct to served: stand the service up over the same bucket,
give it one IAM identity, and swap each client's configuration. Nothing in
the bucket changes.
