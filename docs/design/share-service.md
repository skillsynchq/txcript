# A share service: four designs and the seams under them

Status: **design draft.** No code. `share-store.md` describes the *client*
`Store` and the Worker prototype; this describes the *service* that backs it.

## What this has to be

- Stores transcripts in S3 or another blob store.
- Exposes them to authorized users: publish your own, read everyone's,
  modify only your own.
- Authorization is Cloudflare Access / Zero Trust **today**, pluggable **by
  construction** — not "we could swap it later".
- Deployable to a real host from a NixOS flake, with **auth and config
  deployment separate from the core app**. The core app is the service and
  nothing else.
- Every seam testable, with **at least two implementations beyond the primary
  one**, so each abstraction is shaped by more than one case.

Assumed: Rust, matching the rest of the repo. The payload is opaque to the
service — it validates shape, never interprets content.

---

# Part 1 — The seams

This is the part that matters. The four designs differ in how they *compose*
these; the seams are common to all four.

The rule per seam: **one primary implementation, two alternates that differ
in kind, and one test double.** Two alternates is the minimum that stops an
interface from being a description of its first implementation. Where the
alternates differ only cosmetically, the seam is in the wrong place.

## Seam 1: `Identity` — who is this request from?

```rust
/// Extracts a verified principal from request headers. Never reads the body,
/// never authorizes — only "who".
pub trait Identity: Send + Sync {
    /// `Ok(None)` = no credentials presented, which the caller turns into
    /// 401. `Err` = the check itself failed (JWKS unreachable) → 503. The
    /// distinction matters for alerting: one is a user, one is an outage.
    fn principal(&self, headers: &Headers) -> Result<Option<Principal>, IdentityError>;
}

pub struct Principal {
    /// Stable, opaque, injective. NOT an email — see the collision bug in
    /// share-store.md. Derived by the impl, only ever compared by the service.
    pub id: PrincipalId,
    /// Display only. Never used for authorization or key derivation.
    pub label: Option<String>,
    /// Coarse kind: a service token is a robot and may warrant different
    /// write rules than a human.
    pub kind: PrincipalKind, // Human | Service
}
```

| impl | how it identifies | why it differs *in kind* |
|---|---|---|
| **`CloudflareAccess`** (primary) | verifies `Cf-Access-Jwt-Assertion` against team JWKS + AUD | asymmetric sig, remote key rotation, `email` vs `common_name` |
| `OidcBearer` (alt 1) | `Authorization: Bearer` JWT via OIDC discovery | same crypto, different header and claim mapping — forces the header name out of the impl |
| `MutualTls` (alt 2) | client cert subject forwarded by the terminating proxy | no JWT at all; identity arrives as a bare string — forces "verify" to stop meaning "parse a JWT" |
| `StaticTokens` (double) | token → principal map from config | no network; makes every other seam's tests runnable offline |

**What the alternates force out:** `MutualTls` earns its keep. With only the
two JWT impls, `principal()` would drift toward returning claims and JWKS
caching would leak into the trait. With mTLS in the set the trait can only be
"headers in, principal out".

## Seam 2: `ObjectStore` — where do bytes live?

```rust
pub trait ObjectStore: Send + Sync {
    async fn put(&self, key: &Key, body: Bytes, meta: &ObjectMeta, pre: Precondition)
        -> Result<Version, StoreError>;
    async fn get(&self, key: &Key) -> Result<Option<Object>, StoreError>;
    async fn head(&self, key: &Key) -> Result<Option<ObjectMeta>, StoreError>;
    async fn delete(&self, key: &Key, pre: Precondition) -> Result<(), StoreError>;
    /// Paginated; metadata only, never bodies.
    async fn list(&self, prefix: &Key, cursor: Option<Cursor>) -> Result<Page, StoreError>;
}

/// Optimistic concurrency, expressed once so every impl must answer it.
pub enum Precondition { None, IfAbsent, IfVersion(Version) }
```

| impl | backing | why it differs *in kind* |
|---|---|---|
| **`S3`** (primary) | any S3-compatible: AWS, R2, MinIO, Ceph | the target; conditional writes via `If-Match` / `If-None-Match` |
| `Filesystem` (alt 1) | a directory tree | no native LIST-with-metadata, no atomic CAS — forces preconditions to be expressible without S3 semantics and metadata to have a defined home |
| `Postgres` (alt 2) | `bytea` + metadata table | transactional; makes "list is eventually consistent" an S3 quirk rather than an assumption baked into callers |
| `InMemory` (double) | `BTreeMap` | deterministic ordering; fault injection |

**What the alternates force out:** `Filesystem` kills the assumption that
LIST returns user metadata inline — the single biggest unverified assumption
in the current Worker. If a directory can satisfy the interface, metadata
storage becomes an explicit decision instead of an S3 accident.

## Seam 3: `Policy` — may this principal do this?

Deliberately separate from `Identity`. Conflating them is what produced the
prefix-collision bug: ownership was implied by a *string transformation*
instead of decided by a *rule*.

```rust
pub trait Policy: Send + Sync {
    fn authorize(&self, who: &Principal, action: Action, target: &Target) -> Decision;
    /// The prefix a listing is restricted to, or None for everything.
    fn list_scope(&self, who: &Principal, scope: ListScope) -> Option<Key>;
}

pub enum Action { Read, Publish, Overwrite, Delete }
pub enum Decision { Allow, Deny(&'static str) }
```

| impl | rule | why it differs *in kind* |
|---|---|---|
| **`OwnerPrefix`** (primary) | key is `<principal>/<id>`; read all, write own | structural — a prefix comparison |
| `ReadOnlyMirror` (alt 1) | everyone reads, nobody writes | forces `Publish`/`Overwrite`/`Delete` to stay distinct rather than collapsing into one write bit |
| `TeamScoped` (alt 2) | read within your team, write your own | the key alone no longer determines the answer — forces `Target` to carry metadata and kills any `starts_with` shortcut in the service |
| `AllowAll` (double) | everything | isolates store/transport bugs from policy bugs |

**What the alternates force out:** `TeamScoped` is the important one. With
only `OwnerPrefix` the service would keep a `key.starts_with(principal)`
somewhere and the trait would be decoration. A policy whose answer depends on
object metadata forces the decision to be genuinely delegated.

## Seam 4: `Catalog` — how is "list everyone's sessions" answered?

Separated because its cost model differs wildly between designs — it is the
reason A and B diverge.

```rust
pub trait Catalog: Send + Sync {
    async fn upsert(&self, entry: &Entry) -> Result<(), CatalogError>;
    async fn remove(&self, key: &Key) -> Result<(), CatalogError>;
    async fn query(&self, q: &Query) -> Result<Vec<Entry>, CatalogError>;
}
```

| impl | how | why different |
|---|---|---|
| **`StoreDerived`** (primary) | LIST the bucket, read object metadata | no second source of truth, no sync problem |
| `Sqlite` (alt 1) | local index, rebuildable from the store | introduces staleness — forces `Entry` to be reconstructible from the store alone |
| `Postgres` (alt 2) | shared index across replicas | forces the trait to be safe across processes and to not assume a single writer |
| `InMemory` (double) | vec | — |

## Seam 5: `Config` — and the deployment boundary

The core app must not know the word "Cloudflare". It takes one typed config
and a set of constructed impls:

```rust
pub struct ServiceConfig {
    pub listen: SocketAddr,
    pub identity: IdentityConfig,   // tagged enum
    pub store: StoreConfig,         // tagged enum
    pub policy: PolicyConfig,       // tagged enum
    pub limits: Limits,
}
```

- **The binary** reads one config file path and nothing else. No env sniffing
  for credentials, no implicit `~/.aws`. Secrets arrive as *file paths* read
  at startup, never as env values — so systemd `LoadCredential`, sops, and
  agenix all work without the app knowing they exist.
- **The flake** ships `packages.default` (the binary, auth-agnostic) and
  `nixosModules.service` (systemd unit + config generation).
- **A separate module** `nixosModules.cloudflare-access` wires `cloudflared`
  as its own unit and selects the Cloudflare identity impl. Switching to OIDC
  means importing a different module; the service package is unchanged and is
  not rebuilt.

```nix
# The separation, concretely:
services.txcript-share = {
  enable = true;
  store = { kind = "s3"; bucket = "txcript"; credentialsFile = config.age.secrets.s3.path; };
  policy = "owner-prefix";
};

# auth is a separate import, separately swappable:
imports = [ txcript.nixosModules.cloudflare-access ];
services.txcript-share.cloudflareAccess = {
  team = "example";
  audFile = config.age.secrets.access-aud.path;
};
```

---

# Part 2 — Four designs

## Design A — Stateless service over the object store

One binary, `Catalog = StoreDerived`. No database, no local state; the bucket
is the entire truth. Listing is a LIST plus whatever metadata comes back.

- **Deploy:** systemd unit behind `cloudflared`. Restart loses nothing;
  horizontal scaling is free because there is no local state.
- **Good at:** the simplest thing with all the seams present. Recovery is
  "point it at the bucket". Nothing to migrate or rebuild.
- **Bad at:** listing cost grows with the bucket, and rich queries (by cwd,
  branch, full text) are impossible without downloading bodies. S3 LIST is
  eventually consistent, so a just-published session can be missing from the
  next listing.
- **Pick if:** sessions number in the thousands and listing is id/title/time.

## Design B — Service plus a local index

Design A with `Catalog = Sqlite`. The store stays the source of truth; the
index is a cache, droppable and rebuildable by walking the bucket.

- **Deploy:** same unit plus a state directory. The rebuild path must be a
  first-class command (`--reindex`), not a maintenance script, or it rots.
- **Good at:** fast listing regardless of bucket size, and real queries —
  filter by cwd, branch, model, and later full-text over extracted
  transcripts, which is what `txcript query` already does locally.
- **Bad at:** two sources of truth. Every write becomes two writes, and
  "index says yes, store says gone" must be handled on read.
- **Pick if:** listing is the primary operation — which for this feature it
  explicitly is.

## Design C — Sans-IO core, several hosts

The core crate is pure: request description in, decision out. No I/O, no
async. Hosts adapt it — a native binary, and a wasm build running as a
Cloudflare Worker over R2.

```rust
// No await, no sockets. Fully deterministic.
pub fn decide(req: &Request, who: &Principal, policy: &dyn Policy) -> Plan;

pub enum Plan {
    Reject(Status, &'static str),
    ReadObject(Key),
    WriteObject(Key, ObjectMeta),
    ListPrefix(Key),
}
```

- **Deploy:** the flake builds the native host; wrangler deploys the Worker
  from the same core. Both hosts are thin.
- **Good at:** testability. The entire authorization surface is a pure
  function, so read-vs-read/write is table-driven with no server at all. It
  also keeps the existing Worker as a real target instead of discarding it.
- **Bad at:** an indirection that only pays off if you actually ship both
  hosts. If the Worker is abandoned this is Design A with ceremony.
- **Pick if:** you want the edge deployment *and* a self-hosted one from a
  single implementation of the rules.

## Design D — Control plane only, presigned data plane

The service authenticates, authorizes, and returns a **short-lived presigned
S3 URL**. Bodies never pass through the app: clients PUT and GET directly
against the bucket.

- **Deploy:** a much smaller service — no body handling, no size caps, no
  streaming. `POST /publish` returns a presigned PUT; `GET /s/<slug>` 302s to
  a presigned GET.
- **Good at:** the app never sees a 10 MB transcript, so memory stays flat
  and large sessions are free. Fewest moving parts at runtime.
- **Bad at:** a presigned URL is a **bearer capability** — once issued it
  works for anyone holding it, for its full lifetime, outside your policy.
  Content validation becomes impossible (you never see the bytes, so you
  cannot check it is a Simple document) and the bucket accumulates junk.
  `ObjectStore` grows a `presign()` that `Filesystem` cannot implement
  honestly, which is a signal the seam is being bent.
- **Pick if:** transcripts get large enough that egress through the app is
  the bottleneck. Not before.

---

# Part 3 — How this gets verified

## The conformance suite is the anti-overfitting device

One suite per seam, written against the **trait**, run against **every**
implementation including the doubles. A seam is only as good as its worst
passing implementation.

```rust
// Runs for S3 (Silo in a container), Filesystem, Postgres, InMemory.
store_conformance!(S3, Filesystem, Postgres, InMemory);
```

Required cases:

- **`ObjectStore`**: put/get round trip; `IfAbsent` rejects an existing key;
  `IfVersion` rejects a stale version; delete is idempotent; list paginates
  and returns metadata; keys with `/`, unicode, and maximum length.
- **`Identity`**: valid credential → principal; absent → `Ok(None)`;
  malformed → `Ok(None)` not `Err`; expired → `Ok(None)`; backend down →
  `Err`; and **two distinct inputs never produce the same `PrincipalId`**,
  as a property test.
- **`Policy`**: the matrix below, for every impl.
- **`Catalog`**: upsert/query round trip; remove; rebuild-from-store equals
  live state.

## The access matrix — the test you asked for

Run for every `(Identity, Policy)` pair. `A` and `B` are distinct principals.

| actor | action | target | `OwnerPrefix` | `ReadOnlyMirror` | `TeamScoped` |
|---|---|---|---|---|---|
| anonymous | read | anything | 401 | 401 | 401 |
| A | publish | own | 201 | 403 | 201 |
| A | read | own | 200 | 200 | 200 |
| A | read | B's | 200 | 200 | 200 same team, else 403 |
| A | overwrite | own | 200 | 403 | 200 |
| **A** | **overwrite** | **B's** | **403** | **403** | **403** |
| **A** | **delete** | **B's** | **403** | **403** | **403** |
| A | delete | own | 204 | 403 | 204 |
| A | list | all | own + B's | own + B's | team only |

The two bold rows are the security property. They must be asserted **against
the store**, not just the status code: after a denied overwrite, re-read the
object and assert the bytes are unchanged. A 403 with a mutated object is
exactly the failure this catches, and a status-only assertion misses it.

Two cases outside the matrix that must not be forgotten:

- **Principal collision**: property test that `PrincipalId` is injective over
  arbitrary identity strings. This bug already happened once.
- **Capability confusion** (Design D only): a presigned URL issued for A's
  key must not be usable to write B's key, and must expire.

## Deployment verification

A NixOS VM test (`nixosTest`) brings the service up with `StaticTokens` and
`Filesystem` and runs the access matrix over real HTTP against a real systemd
unit. That validates the module, the config generation, and the secret
plumbing with no Cloudflare and no S3 in CI.

Cloudflare Access itself is verified once, manually, against a staging tenant
— the irreducible part:

- an unauthenticated request gets a **302 to SSO, not a 401**
- a service token authenticates and maps to a `common_name` principal
- a human SSO login maps to an `email` principal, distinct from the above

---

# Recommendation

**Design B, over the Design C core if the Worker survives.**

Listing other people's sessions is the stated point of the feature — exactly
what A is worst at and B is best at. D optimizes a bottleneck that does not
exist yet and weakens the authorization story to do it.

Build order, which front-loads the anti-overfitting work:

1. The seams and their doubles. No S3, no Cloudflare.
2. Conformance suites and the access matrix, green against `InMemory` +
   `StaticTokens` + all three policies.
3. `S3` and `CloudflareAccess`, which must pass the same suites unchanged.
   **If either needs a trait widened, the seam was wrong** — that is the
   signal to stop and reshape rather than add a parameter.
4. `Filesystem` and `OidcBearer` as the overfit check. Cheap, and the real
   test of whether the abstractions hold.
5. The flake: package, service module, separate auth module, `nixosTest`.
6. `Sqlite` catalog once listing is slow enough to notice.
