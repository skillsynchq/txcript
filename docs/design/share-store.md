# `share` — a remote Store for publishing and listing transcripts

Status: **built.** The client `Store` is `src/harness/share.rs`, reached from
the CLI as `txcript push` and `txcript pull`, and `share` joins aggregate
discovery so `txcript list` includes what others published. This is the
design record; to use it, see [deploy.md](../deploy.md).

Two things it needs from the library first, neither of which exists yet:
`http::Agent` has no plain-client constructor (it always applies the
Chrome148 browser profile, which a service we own neither needs nor wants)
and is GET-only, while publishing needs PUT and DELETE.

## Shape

`share` is a *pseudo-harness* in the mould of `campfire.rs`: it reuses another
module's `Body`, `Codec`, and `TextCodec` verbatim and changes only its
identity and its storage. Campfire delegates to `pi` and swaps the sessions
root; `share` delegates to `simple` and swaps a directory for a URL.

That choice follows from what `simple.rs` already says about itself — it is
the one format with deliberately no `Store`, because a Simple session is "a
document handed to txcript directly … not something discovered from or
written into a managed location". `share` is exactly that document at rest
behind a URL, so the conversion core needs no changes at all.

Listing other people's sessions is the point of the feature, so `discover()`
is the primary operation, not an afterthought: the Worker's `GET /s` returns
metadata only (R2 custom metadata, one round trip per 1000 sessions) and
never document bodies.

## Access control

Not implemented here, by design. The service sits behind Cloudflare Access;
the client's entire auth surface is a header map. **Without that external
gate the service is public even over HTTPS** — see the SECURITY note at the
foot of `deploy/cloudflare/src/index.js`.

## `src/harness/share.rs`

```rust
//! share — publish transcripts to an HTTP service, and list what others
//! have published.
//!
//! A thin delegate over [`simple`]: the body, codec, and text codec are
//! Simple's, reused verbatim; only the identity and the store differ.
//!
//! Authorization is deliberately absent from this module. The service is
//! expected to sit behind an authenticating proxy (Cloudflare Access, and
//! anything else that speaks in headers); this store only attaches the
//! credentials an [`Auth`] provider hands it. An unauthenticated endpoint
//! is a public endpoint, HTTPS or not.

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::harness::simple::{self, Doc, Simple};
use crate::transcript::{
    Codec, Common, Discovered, Harness, Saved, Store, TextCodec, Transcript,
};

/// The Share harness marker. Shares Simple's native [`Doc`] body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Share;

impl Harness for Share {
    const NAME: &'static str = "share";
    type Body = Doc;
}

impl Codec for Share {
    fn to_common(transcript: &Transcript<Self>) -> Result<Transcript<Common>> {
        Simple::to_common(&Transcript::<Simple>::new(
            transcript.meta.clone(),
            transcript.body.clone(),
        ))
    }

    fn from_common(transcript: &Transcript<Common>) -> Result<Transcript<Self>> {
        let native = Simple::from_common(transcript)?;
        Ok(Transcript::new(native.meta, native.body))
    }
}

impl TextCodec for Share {
    fn from_text(text: &str) -> Result<Transcript<Self>> {
        let native = Simple::from_text(text)?;
        Ok(Transcript::new(native.meta, native.body))
    }

    fn to_text(transcript: &Transcript<Self>) -> Result<String> {
        Simple::to_text(&Transcript::<Simple>::new(
            transcript.meta.clone(),
            transcript.body.clone(),
        ))
    }
}

// --- auth -------------------------------------------------------------

/// Credentials for the service, as request headers.
///
/// Headers are the whole abstraction on purpose: a Cloudflare Access
/// service token is two of them, and so is almost every other gateway's
/// scheme. Callers with an exotic setup implement this rather than
/// patching the store.
///
/// Called per request, so a provider may refresh a short-lived token.
pub trait Auth: Send + Sync {
    /// # Errors
    /// When credentials are configured but cannot be produced.
    ///
    /// `Cow` because `http::Request` names headers with `&'static str`
    /// today, which cannot carry a name read from the environment at
    /// runtime. Widening the transport's header name type to
    /// `Cow<'static, str>` is the smallest change that admits both; the two
    /// existing call sites pass literals, which coerce to `Cow::Borrowed`.
    fn headers(&self) -> Result<Vec<(Cow<'static, str>, String)>>;

    /// Whether this provider can authenticate writes. A reader-only
    /// configuration makes `save`/`delete` fail fast with a clear message
    /// instead of a 403 from the far end.
    fn can_write(&self) -> bool {
        true
    }
}

/// No credentials: read whatever the endpoint serves anonymously.
pub struct Anonymous;

impl Auth for Anonymous {
    fn headers(&self) -> Result<Vec<(Cow<'static, str>, String)>> {
        Ok(Vec::new())
    }
    fn can_write(&self) -> bool {
        false
    }
}

/// A Cloudflare Access service token — `CF-Access-Client-Id` and
/// `CF-Access-Client-Secret`, read from the environment.
pub struct CfServiceToken {
    client_id: String,
    client_secret: String,
}

impl CfServiceToken {
    /// From `TXCRIPT_SHARE_CF_CLIENT_ID` / `TXCRIPT_SHARE_CF_CLIENT_SECRET`.
    ///
    /// # Errors
    /// When either variable is missing or empty.
    pub fn from_env() -> Result<Self> { /* … */ }
}

impl Auth for CfServiceToken {
    fn headers(&self) -> Result<Vec<(Cow<'static, str>, String)>> {
        Ok(vec![
            ("CF-Access-Client-Id".into(), self.client_id.clone()),
            ("CF-Access-Client-Secret".into(), self.client_secret.clone()),
        ])
    }
}

/// Literal headers from repeated `TXCRIPT_SHARE_HEADER="Name: value"`
/// variables — the escape hatch that lets a bearer token, basic auth, or
/// another gateway work with no code in this repo.
pub struct EnvHeaders(Vec<(Cow<'static, str>, String)>);

/// A command that prints a token on stdout, for interactive SSO:
/// `TXCRIPT_SHARE_TOKEN_CMD="cloudflared access token --app=<url>"`.
/// The output becomes the `cf-access-token` header.
pub struct TokenCommand {
    argv: Vec<String>,
    header: String,
}

// --- store ------------------------------------------------------------

/// A published transcript: its slug (`<owner>/<session-id>`) and the
/// server's ETag, which doubles as the change cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareRef {
    pub slug: String,
    pub etag: Option<String>,
}

impl ShareRef {
    #[must_use]
    pub fn key(&self) -> String {
        self.slug.clone()
    }
}

pub struct ShareStore {
    base_url: String,
    auth: Box<dyn Auth>,
    agent: crate::http::Agent,
}

impl ShareStore {
    /// From `TXCRIPT_SHARE_URL` plus whichever credential variables are set.
    ///
    /// # Errors
    /// When no endpoint is configured or the credentials are malformed.
    pub fn from_env() -> Result<Self> { /* … */ }

    pub fn new(base_url: impl Into<String>, auth: Box<dyn Auth>) -> Result<Self> { /* … */ }

    #[cfg(test)]
    fn for_test(base_url: String) -> Result<Self> { /* … */ }
}

impl Store for ShareStore {
    type H = Share;
    type Ref = ShareRef;

    /// `GET /s` — metadata for every published session, newest first.
    /// Bodies are never fetched here.
    fn discover(&self) -> Result<Vec<Discovered<Self::Ref>>> { /* … */ }

    /// `GET /s/<slug>`.
    fn load(&self, reference: &Self::Ref) -> Result<Transcript<Self::H>> { /* … */ }

    /// `PUT /s/<session-id>`. The service prepends the caller's own
    /// identity prefix, so the client never has to know who it is, and
    /// cannot write outside its namespace.
    fn save(&self, transcript: &Transcript<Self::H>) -> Result<Saved<Self::Ref>> {
        if !self.auth.can_write() {
            return Err(read_only_error());
        }
        /* … */
    }

    /// `DELETE /s/<slug>` — the service refuses another owner's slug.
    fn delete(&self, reference: &Self::Ref) -> Result<()> { /* … */ }

    /// ETags from the last `discover()`. No network, matching the
    /// no-I/O spirit of the file stores' mtime cursors.
    fn fingerprints(&self, refs: &[Self::Ref]) -> Result<HashMap<String, String>> {
        Ok(refs
            .iter()
            .map(|r| (r.key(), r.etag.clone().unwrap_or_default()))
            .collect())
    }
}

fn read_only_error() -> Error {
    Error::Remote {
        harness: Share::NAME,
        detail: "no write credentials configured; the share store is read-only \
                 (set TXCRIPT_SHARE_CF_CLIENT_ID / _SECRET)"
            .to_string(),
    }
}
```

### Three guards that are not optional

1. **Slug validation before it becomes a URL path.** Slugs arrive from the
   service, so the store treats them as untrusted exactly as
   `harness::checked_id_component` treats session ids: two plain segments,
   no `/`-escapes, no `.`/`..`, no control characters. `tests/integration/
   path_safety.rs` is the home for the adversarial cases.
2. **Redirects are errors, not content.** Already handled: `http::Agent`
   builds its client with `redirect(Policy::none())` (`src/http.rs:95`), so
   `share` inherits it and must not re-add it locally. It matters here more
   than for the harnesses it was written for — an unauthenticated request to
   an Access-protected origin gets a **302 to the SSO login page, not a
   401**. Following it would parse an HTML login form as a transcript.
3. **Credentials never reach an error string.** `claude_chat.rs:2499` tests
   this with a token literally named `secret-never-print`, asserting it is
   absent from the rendered error. Mirror it for the Access headers.

## Wiring

The `harness` skill's Phase 5 list, corrected — item 4 names `src/bin/cli.rs`,
which no longer exists; the CLI is a workspace member now.

1. `src/harness/share.rs` — the module above.
2. `src/harness/mod.rs` — `pub mod share;`
3. `src/transcript.rs` — `HarnessId::Share`, `ALL` (17 → 18), `as_str`,
   `FromStr` aliases (`share`, `gist`).
4. `src/local.rs` — `Locator::ShareRemote(ShareRef)` plus arms in
   `discover_harness`, `Session::read`, `Session::delete`, `location()`.
5. `cli/src/lib.rs` — `load_direct_share`, mirroring `load_direct_chatgpt`
   (`:517`) so `txcript view <slug>` is one fetch with no enumeration.
6. `src/wasm.rs` — both dispatch matches, delegating to Simple's codec.
7. `Cargo.toml` — a `share` feature (`dep:wreq`, `dep:tokio`); codec stays
   featureless so the WASM build is unaffected.
8. `README.md`, `docs/usage.md`.
9. Tests, below.

### Two decisions this forces

- **Does `share` appear in a bare `txcript list`?** **Decided: yes, but only
  when `TXCRIPT_SHARE_URL` is set.** `src/local.rs:101` says "Live web
  harnesses are deliberately excluded from aggregate discovery" — claude_chat
  and chatgpt appear only under an explicit `--from`, because discovery would
  otherwise put a network round trip on every `list` and `query`. Listing
  others' sessions is the whole point of `share`, so it is included in
  aggregate discovery — but configuring the endpoint is what opts you in.
  Unset means not configured, which discovers nothing and contacts nobody.
  This keeps the no-config default exactly as fast as it is today.
- **Extract `src/http.rs` first.** Done and merged — `http::Agent` backs both
  `claude_chat` and `chatgpt`, so `share` reuses it instead of adding a third
  copy of the transport. Two things about it shape the `share` work:
  - `Agent::start` hardcodes the Chrome148 emulation profile. `share` talks to
    a service we own and wants none of that fingerprinting — it needs either a
    profile argument or a plain-client constructor. **This is the one library
    change `share` requires, and it should land before the store.**
  - `Request` already splits `headers` from `sensitive`, which is exactly the
    shape an `Auth` provider wants: credentials go in `sensitive`, everything
    else in `headers`. Both name headers with `&'static str` though, so a
    header name read from an env var at runtime cannot be passed without
    leaking it. Widen both to `Cow<'static, str>`; the existing call sites
    pass literals and coerce unchanged.
  - `Agent::get` is GET-only. Publishing needs PUT and DELETE, so the agent
    grows a method (or a verb field) as part of the same change.

## Tests

`tests/README.md` requires integration tests to use real backing stores and
no mocks. For an HTTP store the honest analogue is a real server on a
loopback port, which both remote stores already do:
`TcpListener::bind("127.0.0.1:0")` at `chatgpt.rs:939` and
`claude_chat.rs:2113`, driven through a `for_test(base_url)` constructor.

| Test | Proves |
|---|---|
| `store_round_trip_is_lossless` | `save` → `load` preserves the document, unknown top-level keys included |
| `discover_extracts_metadata` | every populated `Meta` field survives the R2 custom-metadata projection |
| `discover_pages_through_a_truncated_listing` | the cursor loop terminates and concatenates in order |
| `save_without_credentials_is_refused_locally` | `Anonymous` fails fast, no request sent |
| `a_login_redirect_is_an_auth_error_not_a_parse_error` | the Access 302 case |
| `slug_traversal_is_rejected` | `../` and absolute slugs cannot escape the path (`path_safety.rs`) |
| `fingerprints_come_from_etags_without_a_request` | the search cache skips unchanged sessions |
| `credentials_never_appear_in_an_error` | mirrors `claude_chat.rs:2499` |

The codec fixpoint and `cross_harness` hop are inherited from Simple and add
nothing here — `properties.rs` already sweeps that codec. The value in this
integration is entirely at the Store boundary.
